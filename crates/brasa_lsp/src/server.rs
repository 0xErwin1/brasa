//! The server loop: stdio transport, document state, and the two
//! requests this server answers.
//!
//! # What it advertises, and what it does not
//!
//! `textDocumentSync: FULL` and `hoverProvider`. Nothing else. Full
//! sync rather than incremental because the analysis re-runs from
//! scratch anyway (see [`crate::analysis`]) — accepting incremental
//! edits would mean maintaining a rope to feed a pipeline that wants
//! the whole string back, which is work bought for nothing.
//!
//! # One document, one analysis
//!
//! Each open document is analysed as its own entry point, and its
//! imports are followed from disk with the open buffers overlaid. That
//! is wrong for a library file opened on its own — it is analysed as if
//! it were a script, so anything its importer would have given it is
//! missing — and right for the common case, which is editing a script.
//! Picking an entry per workspace is what a project manifest would
//! decide, and Brasa does not have one yet.

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use brasa_module::Overlay;
use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    PublishDiagnostics,
};
use lsp_types::request::{HoverRequest, Request as RequestTrait};
use lsp_types::{
    Hover as LspHover, HoverContents, HoverProviderCapability, MarkupContent, MarkupKind,
    PublishDiagnosticsParams, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri,
};

use crate::analysis::{self, Analysis};
use crate::convert;

/// Runs the server over stdio until the client says to stop.
pub fn run() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    })?;

    connection.initialize(capabilities)?;
    Server::default().serve(&connection)?;

    // The connection has to go before the threads are joined. Joining
    // waits for the writer to finish, and the writer finishes when its
    // channel closes — which cannot happen while a `Connection` still
    // holds the sending half. Keeping it alive here hangs the process
    // after `exit`, with the editor already gone.
    drop(connection);

    io_threads.join()?;
    Ok(())
}

/// Every open document's text, keyed the way the client names it.
#[derive(Default)]
struct Server {
    open: HashMap<Uri, String>,
}

impl Server {
    fn serve(&mut self, connection: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
        for message in &connection.receiver {
            match message {
                Message::Request(request) => {
                    if connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    self.request(connection, request)?;
                }
                Message::Notification(notification) => {
                    self.notification(connection, notification)?;
                }
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn request(
        &mut self,
        connection: &Connection,
        request: Request,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            HoverRequest::METHOD => match cast::<HoverRequest>(request) {
                Ok((_, params)) => {
                    let position = params.text_document_position_params;
                    serde_json::to_value(
                        self.hover(&position.text_document.uri, position.position),
                    )?
                }
                Err(_) => serde_json::Value::Null,
            },
            // A request this server did not advertise. Answering null
            // rather than an error keeps a client that asks anyway from
            // showing the user a failure for a feature that is simply
            // absent.
            _ => serde_json::Value::Null,
        };

        connection
            .sender
            .send(Message::Response(Response::new_ok(id, result)))?;
        Ok(())
    }

    fn notification(
        &mut self,
        connection: &Connection,
        notification: lsp_server::Notification,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: lsp_types::DidOpenTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                self.open.insert(uri.clone(), params.text_document.text);
                self.publish(connection, &uri)?;
            }
            DidChangeTextDocument::METHOD => {
                let params: lsp_types::DidChangeTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;

                // FULL sync: the last change carries the whole document.
                if let Some(change) = params.content_changes.into_iter().next_back() {
                    self.open.insert(uri.clone(), change.text);
                }
                self.publish(connection, &uri)?;
            }
            DidCloseTextDocument::METHOD => {
                let params: lsp_types::DidCloseTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                self.open.remove(&uri);

                // Clear what we said about it: the file may still be on
                // disk and fine, and a closed document's squiggles
                // would otherwise outlive any way to see them.
                self.send_diagnostics(connection, &uri, Vec::new())?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The overlay for one analysis: every open buffer, so a file
    /// imported by the one being edited is seen as the user has it
    /// rather than as it was last saved.
    fn overlay(&self) -> Overlay {
        let mut overlay = Overlay::new();
        for (uri, text) in &self.open {
            if let Some(path) = path_of(uri) {
                overlay.insert(path, text.clone());
            }
        }
        overlay
    }

    fn analyze(&self, uri: &Uri) -> Option<(PathBuf, Analysis)> {
        let path = path_of(uri)?;
        let analysis = analysis::analyze(&path, &self.overlay());
        Some((path, analysis))
    }

    fn publish(
        &mut self,
        connection: &Connection,
        uri: &Uri,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let Some((path, analysis)) = self.analyze(uri) else {
            return Ok(());
        };

        // Only what belongs to THIS document. One analysis reports on
        // the whole import graph, and a diagnostic in an imported file
        // published against the importer would point at a line the user
        // is not looking at.
        let Some(file) = analysis.file_of(&path) else {
            return Ok(());
        };

        let diagnostics: Vec<_> = analysis
            .diagnostics
            .iter()
            .filter(|diag| diag.primary_span.file == file)
            .map(|diag| {
                convert::diagnostic(&analysis.sources, diag, |span| uri_of(&analysis, span.file))
            })
            .collect();

        self.send_diagnostics(connection, uri, diagnostics)
    }

    fn send_diagnostics(
        &self,
        connection: &Connection,
        uri: &Uri,
        diagnostics: Vec<lsp_types::Diagnostic>,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version: None,
        };

        connection
            .sender
            .send(Message::Notification(lsp_server::Notification::new(
                PublishDiagnostics::METHOD.to_string(),
                params,
            )))?;
        Ok(())
    }

    fn hover(&self, uri: &Uri, position: lsp_types::Position) -> Option<LspHover> {
        let (path, analysis) = self.analyze(uri)?;
        let file = analysis.file_of(&path)?;

        let text = &analysis.sources.get(&file).text;
        let offset = convert::position_to_offset(text, position);

        let hover = analysis.hover(file, offset)?;
        let mut lines = Vec::new();

        if let Some(ty) = &hover.ty {
            lines.push(format!("```brasa\n{ty}\n```"));
        }
        if let Some(throws) = &hover.throws {
            lines.push(format!("```brasa\n{throws}\n```"));
        }
        if lines.is_empty() {
            return None;
        }

        Some(LspHover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: lines.join("\n"),
            }),
            range: Some(convert::span_to_range(&analysis.sources, hover.span)),
        })
    }
}

/// The `file:` URI a loaded file should be reported under.
fn uri_of(analysis: &Analysis, file: brasa_source::FileId) -> Option<Uri> {
    crate::uri::from_path(&analysis.sources.get(&file).path)
}

/// The path a `file:` URI names. `None` for any other scheme, which
/// this server has nothing to say about.
fn path_of(uri: &Uri) -> Option<PathBuf> {
    crate::uri::to_path(uri)
}

fn cast<R>(request: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: RequestTrait,
    R::Params: serde::de::DeserializeOwned,
{
    request.extract(R::METHOD)
}

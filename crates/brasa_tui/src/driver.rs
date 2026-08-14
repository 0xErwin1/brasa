//! The wire between a debug session and the model.
//!
//! Lives here rather than in the binary so it can be tested: this is
//! where a session's answer becomes something the view can draw, and
//! getting it wrong is invisible until someone is looking at a screen.

use brasa_source::SourceMap;
use brasa_vm::debug::{Session, Stop};

use crate::debugger::{Debugger, Frame, Local, Run};

/// Moves the model to what the session just answered.
pub fn apply(debugger: &mut Debugger, session: &Session<'_>, sources: &SourceMap, stop: &Stop) {
    debugger.frames = session
        .frames()
        .iter()
        .map(|frame| {
            let (line, _) = sources.display_line_col(&frame.span.file, frame.span.start);

            Frame {
                name: frame.name.clone(),
                line: line as usize,
                locals: frame
                    .locals
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, view)| {
                        view.as_ref().map(|view| Local {
                            slot,
                            value: view.summary.clone(),
                            children: view.children.clone(),
                            inspectable: view.cell.is_some(),
                        })
                    })
                    .collect(),
            }
        })
        .collect();

    debugger.heap = Some(session.heap().into());

    match stop {
        Stop::Paused { span, .. } => {
            debugger.run = Run::Paused;
            let (line, _) = sources.display_line_col(&span.file, span.start);
            debugger.stopped_at(line as usize);
        }
        Stop::Finished(outcome) => {
            debugger.run = Run::Finished(describe(outcome));
            debugger.current_line = None;
            debugger.frames.clear();
        }
    }
}

/// Describes how a run ended, in the words the status line uses.
pub fn describe(outcome: &brasa_runtime::Outcome) -> String {
    match outcome {
        brasa_runtime::Outcome::Success => "ran cleanly".to_string(),
        brasa_runtime::Outcome::Error { message } => format!("error: {message}"),
        brasa_runtime::Outcome::Panic { message } => {
            format!("panic: {}", message.lines().next().unwrap_or(message))
        }
        brasa_runtime::Outcome::Exit { code } => format!("exit {code}"),
        brasa_runtime::Outcome::BrokenPipe => "output closed".to_string(),
    }
}

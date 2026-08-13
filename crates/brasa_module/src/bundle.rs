//! Delivering a program as one file: the resolved module graph as
//! bytes, and back into a [`Program`] without touching the filesystem.
//!
//! A Brasa program is a set of files, so handing one to a machine that
//! has never seen Brasa means handing over every file it reaches plus
//! the answer to every question the loader asked about where they live.
//! Two decisions shape the format:
//!
//! - **Source, not bytecode.** The compiled module is in-memory only —
//!   bytecode is never serialized (`docs/spec/07-bytecode.md`) — so the
//!   bundle carries the source of every module and compiles it at
//!   startup. That keeps the format independent of the opcode set, the
//!   value representation and the shape tables, none of which are
//!   stable.
//! - **What resolution decided, not what it was asked.** `import
//!   text::slug` resolves against `BRASA_PATH` and a `lib` directory
//!   beside the executed file. Re-running that on the target machine
//!   would make a delivered tool depend on the environment it landed
//!   in, which is the exact failure this exists to prevent. The bundle
//!   stores the edges the loader already resolved, so
//!   [`load`] performs no path resolution at all.
//!
//! An import edge cannot be stored as an `ItemId`: those are handed out
//! by lowering and mean nothing across a serialization boundary. It is
//! stored instead as a position among the module's file-naming imports
//! in source order, which [`crate::file_import_roots`] defines for both
//! sides.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use brasa_diagnostics::Severity;
use brasa_hir::Lowerer;
use brasa_source::SourceMap;

use crate::{Module, Program, file_import_roots};

/// Marks the start of an encoded payload; the format version follows it
/// so a later format can be told apart from a corrupt one.
const PAYLOAD_MAGIC: [u8; 8] = *b"BRASAPKG";

const PAYLOAD_VERSION: u32 = 1;

/// Marks a payload appended to an executable. Distinct from
/// [`PAYLOAD_MAGIC`] on purpose: this one answers "is this binary
/// bundled at all", which is asked on every cold start, while the
/// payload magic answers "is this a payload this build understands".
const TRAILER_MAGIC: [u8; 8] = *b"BRASABND";

/// The size of the record appended after the payload. Fixed, so
/// finding it is one seek to the end of the file rather than a scan.
pub const TRAILER_LEN: usize = 16;

/// One module as it travels: its source and the identity it keeps, with
/// no promise that its path still exists anywhere.
#[derive(Debug, Clone)]
pub struct BundledModule {
    /// The canonical path the module was loaded from on the bundling
    /// machine. Kept for diagnostics only; nothing reads it back off
    /// the filesystem.
    pub path: PathBuf,
    /// The name this module binds in an importer's scope. Stored rather
    /// than derived from `path`, because the path is not guaranteed to
    /// be meaningful on the target.
    pub name: String,
    pub source: String,
    /// Each resolved import, as `(position among this module's
    /// file-naming imports, index into [`Bundle::modules`])`.
    pub imports: Vec<(u32, u32)>,
}

/// A whole program, resolved and ready to be written out.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// Post-order DFS from the entry, the same order
    /// [`Program::modules`] uses.
    pub modules: Vec<BundledModule>,
    pub entry: u32,
}

/// Why a payload could not be produced or read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    /// A module was larger than the format's 32-bit lengths allow.
    TooLarge,
    /// The payload ended in the middle of a field.
    Truncated,
    /// The leading magic is not a Brasa payload.
    NotAPayload,
    /// A payload from a format this build does not know.
    UnsupportedVersion(u32),
    /// A string field was not valid UTF-8.
    NotUtf8,
    /// A module index or import position points outside the graph.
    Inconsistent,
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "the program is too large to bundle"),
            Self::Truncated => write!(f, "the embedded program is truncated"),
            Self::NotAPayload => write!(f, "the embedded program is not a Brasa payload"),
            Self::UnsupportedVersion(version) => write!(
                f,
                "the embedded program uses bundle format version {version}, which this build does not understand"
            ),
            Self::NotUtf8 => write!(f, "the embedded program contains invalid UTF-8"),
            Self::Inconsistent => write!(f, "the embedded module graph is inconsistent"),
        }
    }
}

impl std::error::Error for BundleError {}

impl Bundle {
    /// Captures a loaded program: every module's source, plus the import
    /// edges the loader resolved.
    ///
    /// `sources` must be the map the program was loaded into; a module's
    /// text is read back from it rather than from disk, so the bundle
    /// holds exactly the bytes that were compiled.
    pub fn capture(program: &Program, sources: &SourceMap) -> Result<Self, BundleError> {
        let entry = u32::try_from(program.entry).map_err(|_| BundleError::TooLarge)?;

        let mut modules = Vec::with_capacity(program.modules.len());

        for module in &program.modules {
            let file_imports = file_import_roots(&program.hir, &module.roots);

            let mut imports = Vec::with_capacity(module.imports.len());
            for (position, root) in file_imports.iter().enumerate() {
                let Some(&target) = module.imports.get(root) else {
                    continue;
                };

                let position = u32::try_from(position).map_err(|_| BundleError::TooLarge)?;
                let target = u32::try_from(target).map_err(|_| BundleError::TooLarge)?;
                imports.push((position, target));
            }

            modules.push(BundledModule {
                path: module.path.clone(),
                name: module.name.clone(),
                source: sources.get(&module.file).text.clone(),
                imports,
            });
        }

        Ok(Self { modules, entry })
    }

    /// Serializes to the payload bytes: magic, format version, then the
    /// graph.
    pub fn encode(&self) -> Result<Vec<u8>, BundleError> {
        let mut out = Vec::new();
        out.extend_from_slice(&PAYLOAD_MAGIC);
        put_u32(&mut out, PAYLOAD_VERSION);
        put_u32(&mut out, self.entry);
        put_len(&mut out, self.modules.len())?;

        for module in &self.modules {
            put_str(&mut out, &module.path.to_string_lossy())?;
            put_str(&mut out, &module.name)?;
            put_str(&mut out, &module.source)?;

            put_len(&mut out, module.imports.len())?;
            for &(position, target) in &module.imports {
                put_u32(&mut out, position);
                put_u32(&mut out, target);
            }
        }

        Ok(out)
    }

    /// Reads back what [`Self::encode`] wrote, rejecting anything it
    /// cannot fully account for rather than running a partial graph.
    pub fn decode(bytes: &[u8]) -> Result<Self, BundleError> {
        let mut reader = Reader { bytes, at: 0 };

        if reader.take(PAYLOAD_MAGIC.len())? != PAYLOAD_MAGIC {
            return Err(BundleError::NotAPayload);
        }

        let version = reader.u32()?;
        if version != PAYLOAD_VERSION {
            return Err(BundleError::UnsupportedVersion(version));
        }

        let entry = reader.u32()?;
        let count = reader.u32()? as usize;

        let mut modules = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let path = PathBuf::from(reader.string()?);
            let name = reader.string()?;
            let source = reader.string()?;

            let edges = reader.u32()? as usize;
            let mut imports = Vec::with_capacity(edges.min(1024));
            for _ in 0..edges {
                let position = reader.u32()?;
                let target = reader.u32()?;
                imports.push((position, target));
            }

            modules.push(BundledModule {
                path,
                name,
                source,
                imports,
            });
        }

        let bundle = Self { modules, entry };
        bundle.validate()?;

        Ok(bundle)
    }

    /// Every index the graph contains points at a module that exists.
    /// Checked once here so the loader can index without a fallible path
    /// on every edge.
    fn validate(&self) -> Result<(), BundleError> {
        let count = u32::try_from(self.modules.len()).map_err(|_| BundleError::TooLarge)?;

        if self.entry >= count {
            return Err(BundleError::Inconsistent);
        }

        let in_range = self
            .modules
            .iter()
            .flat_map(|module| &module.imports)
            .all(|&(_, target)| target < count);

        if in_range {
            Ok(())
        } else {
            Err(BundleError::Inconsistent)
        }
    }
}

/// The record appended after the payload, at the very end of the
/// executable: the magic, then the payload's length.
pub fn trailer(payload_len: u64) -> [u8; TRAILER_LEN] {
    let mut out = [0u8; TRAILER_LEN];
    out[..8].copy_from_slice(&TRAILER_MAGIC);
    out[8..].copy_from_slice(&payload_len.to_le_bytes());
    out
}

/// How long the payload before this trailer is, or `None` when these
/// bytes are not a trailer at all — which is what the last bytes of an
/// ordinary, unbundled `brasa` binary look like.
pub fn payload_len(trailer: &[u8; TRAILER_LEN]) -> Option<u64> {
    if trailer[..8] != TRAILER_MAGIC {
        return None;
    }

    let mut len = [0u8; 8];
    len.copy_from_slice(&trailer[8..]);

    Some(u64::from_le_bytes(len))
}

/// Rebuilds a program from a bundle, reading nothing from disk.
///
/// This walks the stored graph exactly the way [`crate::load`] walks the
/// real one — lower a module, follow its imports in source order, then
/// record it — so the module order, the source-map file order and the
/// `ItemId` numbering all come out the same as they did when the bundle
/// was captured. Only the resolution step is different: an edge is a
/// lookup instead of a search of the filesystem.
pub fn load(bundle: &Bundle, sources: &mut SourceMap) -> Program {
    let mut loader = BundleLoader {
        bundle,
        sources,
        lowerer: Lowerer::new(),
        modules: Vec::new(),
        loaded: HashMap::new(),
        diagnostics: Vec::new(),
    };

    let entry = bundle.entry as usize;
    let entry_ix = loader
        .load_module(entry)
        .unwrap_or_else(|| loader.push_empty(entry));

    let (hir, sugar_origins, lower_diagnostics) = loader.lowerer.finish();

    let mut diagnostics = loader.diagnostics;
    diagnostics.extend(lower_diagnostics);

    Program {
        hir,
        modules: loader.modules,
        entry: entry_ix,
        sugar_origins,
        diagnostics,
    }
}

struct BundleLoader<'a> {
    bundle: &'a Bundle,
    sources: &'a mut SourceMap,
    lowerer: Lowerer,
    modules: Vec<Module>,
    /// Bundle index to the [`Program::modules`] index it produced, so a
    /// module reached twice is lowered once.
    loaded: HashMap<usize, usize>,
    diagnostics: Vec<brasa_diagnostics::Diagnostic>,
}

impl BundleLoader<'_> {
    /// Stands in for a module whose source did not parse. Only the entry
    /// needs one: [`Program::entry`] has to point somewhere even when
    /// the payload is corrupt.
    fn push_empty(&mut self, index: usize) -> usize {
        let bundled = &self.bundle.modules[index];
        let file = self.sources.add_file(bundled.path.clone(), String::new());

        let module = Module {
            path: bundled.path.clone(),
            file,
            name: bundled.name.clone(),
            roots: Vec::new(),
            imports: HashMap::new(),
        };

        self.push(module)
    }

    fn push(&mut self, module: Module) -> usize {
        let ix = self.modules.len();
        self.modules.push(module);
        ix
    }

    fn load_module(&mut self, index: usize) -> Option<usize> {
        if let Some(&ix) = self.loaded.get(&index) {
            return Some(ix);
        }

        let bundled = &self.bundle.modules[index];
        let path = bundled.path.clone();
        let name = bundled.name.clone();
        let source = bundled.source.clone();

        let file = self.sources.add_file(path.clone(), source.clone());

        let parsed = brasa_parser::parse(&source, file);
        let parse_failed = parsed
            .diagnostics
            .iter()
            .any(|diag| diag.severity == Severity::Error);
        self.diagnostics.extend(parsed.diagnostics);

        if parse_failed {
            return None;
        }

        let roots = self.lowerer.lower_file(&parsed.ast, &parsed.roots);
        let imports = self.load_imports(index, &roots);

        let ix = self.push(Module {
            path,
            file,
            name,
            roots,
            imports,
        });
        self.loaded.insert(index, ix);

        Some(ix)
    }

    /// Turns the stored positions back into `ItemId`s and follows them.
    ///
    /// A position that does not name one of this module's file-naming
    /// imports means the payload disagrees with the source it carries;
    /// the edge is dropped, which leaves the binding opaque exactly as a
    /// failed load does on the filesystem path.
    fn load_imports(
        &mut self,
        index: usize,
        roots: &[brasa_hir::ItemId],
    ) -> HashMap<brasa_hir::ItemId, usize> {
        let file_imports = file_import_roots(self.lowerer.hir(), roots);
        let edges = self.bundle.modules[index].imports.clone();

        let mut imports = HashMap::new();
        for (position, target) in edges {
            let Some(&root) = file_imports.get(position as usize) else {
                continue;
            };

            if let Some(ix) = self.load_module(target as usize) {
                imports.insert(root, ix);
            }
        }

        imports
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_len(out: &mut Vec<u8>, len: usize) -> Result<(), BundleError> {
    put_u32(out, u32::try_from(len).map_err(|_| BundleError::TooLarge)?);
    Ok(())
}

fn put_str(out: &mut Vec<u8>, value: &str) -> Result<(), BundleError> {
    put_len(out, value.len())?;
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], BundleError> {
        let end = self.at.checked_add(len).ok_or(BundleError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(BundleError::Truncated)?;
        self.at = end;

        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, BundleError> {
        let raw = self.take(4)?;
        let bytes: [u8; 4] = raw.try_into().expect("take yields exactly four bytes");

        Ok(u32::from_le_bytes(bytes))
    }

    fn string(&mut self) -> Result<String, BundleError> {
        let len = self.u32()? as usize;
        let raw = self.take(len)?;

        String::from_utf8(raw.to_vec()).map_err(|_| BundleError::NotUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bundle {
        Bundle {
            modules: vec![
                BundledModule {
                    path: PathBuf::from("/tmp/util.bras"),
                    name: "util".to_string(),
                    source: "pub def double(x: int): int\n  x * 2\nend\n".to_string(),
                    imports: Vec::new(),
                },
                BundledModule {
                    path: PathBuf::from("/tmp/main.bras"),
                    name: "main".to_string(),
                    source: "import \"util.bras\"\nputs util.double(21)\n".to_string(),
                    imports: vec![(0, 0)],
                },
            ],
            entry: 1,
        }
    }

    #[test]
    fn a_bundle_survives_a_round_trip() {
        let bundle = sample();

        let decoded = Bundle::decode(&bundle.encode().expect("encodes")).expect("decodes");

        assert_eq!(decoded.entry, 1);
        assert_eq!(decoded.modules.len(), 2);
        assert_eq!(decoded.modules[0].name, "util");
        assert_eq!(decoded.modules[1].source, bundle.modules[1].source);
        assert_eq!(decoded.modules[1].imports, vec![(0, 0)]);
    }

    #[test]
    fn a_truncated_payload_is_rejected_rather_than_partly_read() {
        let encoded = sample().encode().expect("encodes");

        let short = &encoded[..encoded.len() - 1];

        assert_eq!(Bundle::decode(short).unwrap_err(), BundleError::Truncated);
    }

    #[test]
    fn foreign_bytes_are_not_mistaken_for_a_payload() {
        assert_eq!(
            Bundle::decode(b"not a brasa payload at all").unwrap_err(),
            BundleError::NotAPayload
        );
    }

    #[test]
    fn an_edge_pointing_outside_the_graph_is_rejected() {
        let mut bundle = sample();
        bundle.modules[1].imports = vec![(0, 7)];

        let encoded = bundle.encode().expect("encodes");

        assert_eq!(
            Bundle::decode(&encoded).unwrap_err(),
            BundleError::Inconsistent
        );
    }

    #[test]
    fn a_trailer_round_trips_and_foreign_bytes_do_not() {
        assert_eq!(payload_len(&trailer(4096)), Some(4096));
        assert_eq!(payload_len(&[0u8; TRAILER_LEN]), None);
    }
}

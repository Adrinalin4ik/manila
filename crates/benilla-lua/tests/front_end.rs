//! The front end against the script host's C Lua: every Lua chunk in the addon corpus and the
//! install's interface files, compiled by both. A chunk compiles in both or fails in both with
//! the same message, and a compiled chunk has, function by function, the C build's constants in
//! its order, its register count, parameters, locals, upvalues and line numbers: the code
//! generator allocated as 5.1's does. Every disagreement is printed.
//!
//! `front_end.digest` keeps the C build's shapes for when there is no C build to ask: a line per
//! compiled chunk, a hash of its source and a hash of the C build's shape. `BENILLA_BLESS=1`
//! rewrites it from the C build; a chunk whose source is not in it (another corpus version) is
//! not checked against it.

use std::path::{Path, PathBuf};

struct Chunk {
    name: String,
    bytes: Vec<u8>,
}

/// `benilla_ui::source::chunk`: one leading UTF-8 BOM and a leading `#` line go.
fn prepare(bytes: &[u8]) -> Vec<u8> {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    match b.first() {
        Some(b'#') => match b.iter().position(|&c| c == b'\n') {
            Some(nl) => b[nl..].to_vec(),
            None => Vec::new(),
        },
        _ => b.to_vec(),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Inline `<Script>` text and every handler body in an XML file, each as its own chunk.
fn xml_chunks(name: &str, bytes: &[u8], out: &mut Vec<Chunk>) {
    let text =
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes))
            .into_owned();
    let Ok(doc) = roxmltree::Document::parse(&text) else {
        return;
    };
    for n in doc.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        let inside_scripts = n.parent().is_some_and(|p| p.tag_name().name() == "Scripts");
        if (tag == "Script" && n.attribute("file").is_none()) || inside_scripts {
            let body: String = n.children().filter_map(|c| c.text()).collect();
            if !body.trim().is_empty() {
                out.push(Chunk {
                    name: format!("{name}:{tag}"),
                    bytes: body.into_bytes(),
                });
            }
        }
    }
}

fn corpus_chunks(out: &mut Vec<Chunk>) -> bool {
    let Some(root) = benilla_formats::addon_corpus() else {
        return false;
    };
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    for f in files {
        let name = f.strip_prefix(&root).unwrap().display().to_string();
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        match ext.as_deref() {
            Some("lua") => out.push(Chunk {
                name,
                bytes: prepare(&bytes),
            }),
            Some("xml") => xml_chunks(&name, &bytes, out),
            _ => {}
        }
    }
    true
}

fn chain_chunks(out: &mut Vec<Chunk>) -> bool {
    let Some(data) = benilla_formats::wow_data() else {
        return false;
    };
    let chain = benilla_formats::open_chain(&data).expect("open the patch chain");
    let mut names: Vec<String> = chain
        .list()
        .expect("list the chain")
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.to_ascii_lowercase().starts_with("interface\\"))
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        let lower = name.to_ascii_lowercase();
        let Ok(bytes) = chain.read(&name) else {
            continue;
        };
        if lower.ends_with(".lua") {
            out.push(Chunk {
                name,
                bytes: prepare(&bytes),
            });
        } else if lower.ends_with(".xml") {
            xml_chunks(&name, &bytes, out);
        }
    }
    true
}

/// One function of a 5.1 `string.dump`, the parts benilla's compiler must match.
#[derive(Debug, PartialEq)]
struct Shape {
    linedefined: u32,
    nups: u8,
    numparams: u8,
    vararg: bool,
    maxstack: u8,
    /// Each instruction's line, runs of one line taken as one: 5.1 spends two instructions on
    /// a test benilla fuses into one.
    lines: Vec<u32>,
    consts: Vec<String>,
    locals: Vec<String>,
    upvals: Vec<String>,
    protos: Vec<Shape>,
}

/// 5.1's `lundump.c` over a dump from this platform (little-endian, 4-byte int, 8-byte size_t).
struct Undump<'a>(&'a [u8]);

impl Undump<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        a
    }
    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn int(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn string(&mut self) -> String {
        let n = u64::from_le_bytes(self.take(8).try_into().unwrap()) as usize;
        let b = self.take(n);
        String::from_utf8_lossy(&b[..n.saturating_sub(1)]).into_owned()
    }
    fn function(&mut self) -> Shape {
        self.string();
        let linedefined = self.int();
        self.int();
        let (nups, numparams, vararg, maxstack) =
            (self.byte(), self.byte(), self.byte(), self.byte());
        let ncode = self.int() as usize;
        let code: Vec<u32> = (0..ncode).map(|_| self.int()).collect();
        let consts = (0..self.int())
            .map(|_| match self.byte() {
                0 => "nil".to_string(),
                1 => format!("{}", self.byte() != 0),
                3 => format!("{:?}", f64::from_le_bytes(self.take(8).try_into().unwrap())),
                _ => format!("{:?}", self.string()),
            })
            .collect();
        let protos: Vec<Shape> = (0..self.int()).map(|_| self.function()).collect();
        let nlines = self.int() as usize;
        let all: Vec<u32> = (0..nlines).map(|_| self.int()).collect();
        // `CLOSURE`'s pseudo-instructions, one per upvalue, are not instructions here.
        let mut lines = Vec::new();
        let mut pc = 0;
        while pc < nlines {
            lines.push(all[pc]);
            let i = code[pc];
            pc += 1;
            if i & 0x3F == 36 {
                pc += protos[(i >> 14) as usize].nups as usize;
            }
        }
        lines.dedup();
        let locals = (0..self.int())
            .map(|_| {
                let n = self.string();
                self.int();
                self.int();
                n
            })
            .collect();
        let upvals = (0..self.int()).map(|_| self.string()).collect();
        Shape {
            linedefined,
            nups,
            numparams,
            // `VARARG_HASARG`: the function declares `arg`.
            vararg: vararg & 1 != 0,
            maxstack,
            lines,
            consts,
            locals,
            upvals,
            protos,
        }
    }
}

fn c_shape(dump: &[u8]) -> Shape {
    let mut u = Undump(dump);
    u.take(12);
    u.function()
}

fn our_shape(p: &benilla_lua::proto::Proto, heap: &benilla_lua::heap::Heap) -> Shape {
    use benilla_lua::value::V;
    Shape {
        linedefined: p.line_defined,
        nups: p.upvals.len() as u8,
        numparams: p.nparams,
        vararg: p.vararg,
        maxstack: (p.maxstack - 1) as u8,
        lines: {
            let mut l = p.lines.clone();
            l.dedup();
            l
        },
        consts: p
            .consts
            .iter()
            .map(|v| match v.unpack() {
                V::Nil => "nil".to_string(),
                V::Bool(b) => format!("{b}"),
                V::Num(n) => format!("{n:?}"),
                V::Str(s) => format!("{:?}", String::from_utf8_lossy(heap.str(s))),
                v => panic!("constant {v:?}"),
            })
            .collect(),
        locals: p
            .locvars
            .iter()
            .map(|l| String::from_utf8_lossy(&l.name).into_owned())
            .collect(),
        upvals: p
            .upval_names
            .iter()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect(),
        protos: p.protos.iter().map(|q| our_shape(q, heap)).collect(),
    }
}

/// The first difference between two shapes, by path.
fn shape_diff(a: &Shape, b: &Shape, path: &str) -> Option<String> {
    if a.protos.len() == b.protos.len() {
        for (i, (x, y)) in a.protos.iter().zip(&b.protos).enumerate() {
            if let Some(d) = shape_diff(x, y, &format!("{path}/{i}")) {
                return Some(d);
            }
        }
    }
    let strip = |s: &Shape| Shape {
        protos: Vec::new(),
        lines: s.lines.clone(),
        consts: s.consts.clone(),
        locals: s.locals.clone(),
        upvals: s.upvals.clone(),
        ..*s
    };
    let (x, y) = (strip(a), strip(b));
    (x != y || a.protos.len() != b.protos.len())
        .then(|| format!("function {path}\n    ours: {x:?}\n    fork: {y:?}"))
}

/// FNV-1a, 64 bits: stable across platforms and Rust versions, which `DefaultHasher` is not.
fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

const DIGEST: &str = "tests/front_end.digest";

/// The digest file: each source hash's shape hash.
fn read_digest() -> std::collections::HashMap<u64, u64> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIGEST);
    std::fs::read_to_string(path)
        .expect("the digest file")
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(a, b)| {
            (
                u64::from_str_radix(a, 16).unwrap(),
                u64::from_str_radix(b, 16).unwrap(),
            )
        })
        .collect()
}

#[test]
fn every_chunk_compiles_as_the_fork_compiles_it() {
    let mut chunks = Vec::new();
    let corpus = corpus_chunks(&mut chunks);
    let chain = chain_chunks(&mut chunks);
    if !corpus && !chain {
        benilla_formats::skipped("no addon corpus and no WoW install", &[]);
        return;
    }
    let lua = mlua::Lua::new();
    let (mut both_ok, mut both_err, mut ours_only, mut fork_only) = (0, 0, Vec::new(), Vec::new());
    let mut msg_diff = Vec::new();
    let mut shape_diff_list = Vec::new();
    let mut heap = benilla_lua::heap::Heap::default();
    let mut digest = std::collections::BTreeMap::new();
    let recorded = read_digest();
    let mut digest_diff = Vec::new();
    for c in &chunks {
        let chunkname = format!("@{}", c.name);
        let ours = benilla_lua::check(&c.bytes, chunkname.as_bytes());
        let fork = lua.load(&c.bytes[..]).set_name(&chunkname).into_function();
        match (ours, fork) {
            (Ok(_), Ok(f)) => {
                both_ok += 1;
                let p = benilla_lua::compile(&c.bytes, chunkname.as_bytes(), &mut heap, 0).unwrap();
                let fork_shape = c_shape(&f.dump(false));
                let ours = our_shape(&p, &heap);
                if let Some(d) = shape_diff(&ours, &fork_shape, "") {
                    shape_diff_list.push(format!("{}: {d}", c.name));
                }
                let src = fnv64(&c.bytes);
                digest.insert(src, fnv64(format!("{fork_shape:?}").as_bytes()));
                if recorded
                    .get(&src)
                    .is_some_and(|&h| h != fnv64(format!("{ours:?}").as_bytes()))
                {
                    digest_diff.push(c.name.clone());
                }
            }
            (Err(a), Err(b)) => {
                both_err += 1;
                let b = match b {
                    mlua::Error::SyntaxError { message, .. } => message,
                    other => other.to_string(),
                };
                if a.to_string() != b {
                    msg_diff.push(format!("{}\n    ours: {}\n    fork: {}", c.name, a, b));
                }
            }
            (Err(a), Ok(_)) => ours_only.push(format!("{}: {}", c.name, a)),
            (Ok(_), Err(b)) => fork_only.push(format!("{}: {}", c.name, b)),
        }
    }
    println!(
        "{} chunks: {both_ok} compile in both, {both_err} fail in both, {} fail only here, {} only in the fork",
        chunks.len(),
        ours_only.len(),
        fork_only.len()
    );
    for l in &ours_only {
        println!("REJECTED HERE ONLY  {l}");
    }
    for l in &fork_only {
        println!("REJECTED BY FORK ONLY  {l}");
    }
    for l in msg_diff.iter().take(40) {
        println!("MESSAGE  {l}");
    }
    println!("{} compiled chunks differ in shape", shape_diff_list.len());
    for l in shape_diff_list.iter().take(20) {
        println!("SHAPE  {l}");
    }
    if std::env::var_os("BENILLA_BLESS").is_some() {
        let lines: String = digest
            .iter()
            .map(|(s, h)| format!("{s:016x} {h:016x}\n"))
            .collect();
        std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(DIGEST), lines)
            .expect("write the digest file");
    }
    println!(
        "{} compiled chunks differ from the digest",
        digest_diff.len()
    );
    for l in digest_diff.iter().take(20) {
        println!("DIGEST  {l}");
    }
    assert!(
        ours_only.is_empty()
            && fork_only.is_empty()
            && msg_diff.is_empty()
            && shape_diff_list.is_empty()
            && digest_diff.is_empty(),
        "the two front ends disagree"
    );
}

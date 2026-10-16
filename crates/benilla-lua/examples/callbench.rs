//! Call micro-benchmarks against the fork, best of five.
use std::time::Instant;
const CASES: &[(&str, &str)] = &[
    ("empty call", "local function f() end for i = 1, 3000000 do f() end"),
    ("arg+ret", "local function f(x) return x end local s = 0 for i = 1, 3000000 do s = s + f(i) end"),
    ("upvalue call", "local f f = function(x) return x end local function g() local s = 0 for i = 1, 3000000 do s = s + f(i) end return s end g()"),
    ("method", "local C = {} C.__index = C function C:m() return self end local o = setmetatable({}, C) for i = 1, 3000000 do o:m() end"),
    ("method on self", "local o = {} function o:m() return self end for i = 1, 3000000 do o:m() end"),
    ("loop only", "local s = 0 for i = 1, 3000000 do s = s + i end"),
];
fn best<F: FnMut()>(mut f: F) -> f64 {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        })
        .fold(f64::MAX, f64::min)
}
fn main() {
    if let Some(only) = std::env::args().nth(1) {
        let src = CASES.iter().find(|c| c.0.contains(&only)).unwrap().1;
        let fork = std::env::args().nth(2).is_some_and(|a| a == "fork");
        for _ in 0..20 {
            if fork {
                let lua = mlua::Lua::new();
                lua.load(src).exec().unwrap();
            } else {
                let mut vm = benilla_lua::new_state();
                vm.exec(src.as_bytes(), b"=b").unwrap();
            }
        }
        return;
    }
    for (name, src) in CASES {
        let ours = best(|| {
            let mut vm = benilla_lua::new_state();
            vm.exec(src.as_bytes(), b"=b").unwrap();
        });
        let fork = best(|| {
            let lua = mlua::Lua::new();
            lua.load(*src).exec().unwrap();
        });
        println!(
            "{name:<16} ours {ours:6.2} ms  fork {fork:6.2} ms  {:.2}x",
            ours / fork
        );
    }
}

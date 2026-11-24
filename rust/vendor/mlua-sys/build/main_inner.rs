use std::env;

cfg_if::cfg_if! {
    if #[cfg(any(feature = "luau", feature = "vendored"))] {
        #[path = "find_vendored.rs"]
        mod find;
    } else {
        #[path = "find_normal.rs"]
        mod find;
    }
}

fn main() {
    #[cfg(all(feature = "luau", feature = "module", windows))]
    compile_error!("Luau does not support `module` mode on Windows");

    #[cfg(any(
        all(feature = "vendored", any(feature = "external", feature = "module")),
        all(feature = "external", any(feature = "vendored", feature = "module")),
        all(feature = "module", any(feature = "vendored", feature = "external"))
    ))]
    compile_error!("`vendored`, `external` and `module` features are mutually exclusive");

    println!("cargo:rerun-if-changed=build");
    // rustudio: the vendored Luau carries engine patches (vendor/luau0-src);
    // rebuild it when they change. All of it: a CodeGen-only patch
    // (luau_codegen_compile_native) left a stale library that failed to link.
    println!("cargo:rerun-if-changed=../luau0-src/luau");
    // ...and each of its source files by name: a directory entry alone did
    // not trigger a rebuild on a Windows build slot (a stale luauvm.lib
    // missing newer patches' symbols failed to link).
    fn each_source(dir: &std::path::Path) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                each_source(&path);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("cpp" | "h" | "hpp" | "c")) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    each_source(std::path::Path::new("../luau0-src/luau"));

    // Check if compilation and linking is handled by external crate
    if cfg!(not(feature = "external")) {
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
        if target_os == "windows" && cfg!(feature = "module") {
            if !std::env::var("LUA_LIB_NAME").unwrap_or_default().is_empty() {
                // Don't use raw-dylib linking
                find::probe_lua();
                return;
            }

            println!("cargo:rustc-cfg=raw_dylib");
        }

        #[cfg(not(feature = "module"))]
        find::probe_lua();
    }
}

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() -> io::Result<()> {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("..")
        .join("..")
        .join("runtime")
        .join("plugins");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut generated =
        String::from("pub(crate) static DEFAULT_PLUGIN_ASSETS: &[(&str, &[u8])] = &[\n");
    for (name, path) in files {
        generated.push_str(&format!(
            "    ({name:?}, include_bytes!({path:?})),\n",
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"))
        .join("plugin_assets.rs");
    fs::write(output, generated)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            let name = path
                .strip_prefix(root)
                .expect("plugin asset is below its root")
                .to_string_lossy()
                .replace('\\', "/");
            files.push((name, path));
        }
    }
    Ok(())
}

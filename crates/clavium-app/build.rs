//! Incluye en el binario todas las piezas de `library/` y las rutas de
//! aprendizaje, para que sumar una pieza sea solo agregar su archivo.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    windows_version_info();

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let library = manifest.join("../../library").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", library.display());

    let mut pieces: Vec<(String, PathBuf)> = std::fs::read_dir(&library)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .filter(|p| p.file_stem().is_some_and(|s| s != "rutas"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    pieces.sort();

    let mut out = String::from("pub const BUNDLED: &[(&str, &str)] = &[\n");
    for (id, path) in &pieces {
        println!("cargo:rerun-if-changed={}", path.display());
        writeln!(
            out,
            "    ({id:?}, include_str!({:?})),",
            path.display().to_string()
        )
        .unwrap();
    }
    out.push_str("];\n");

    let routes = library.join("rutas.txt");
    println!("cargo:rerun-if-changed={}", routes.display());
    writeln!(
        out,
        "pub const ROUTES: &str = include_str!({:?});",
        routes.display().to_string()
    )
    .unwrap();

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bundled.rs");
    std::fs::write(dest, out).unwrap();
}

/// Nombre y versión de producto dentro del `.exe` de Windows. Los pide la firma
/// de código, y Windows los muestra en las propiedades del archivo.
fn windows_version_info() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Clavium")
        .set("FileDescription", "Clavium")
        .set("OriginalFilename", "Clavium.exe")
        .set("LegalCopyright", "Licencia MPL-2.0");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no se pudo agregar la información de versión: {e}");
    }
}

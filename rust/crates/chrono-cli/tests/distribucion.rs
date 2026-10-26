//! Lo que ve quien instala: `install.sh` y los README no hablan del binario Go
//! retirado, y la versión por defecto del instalador es la del Cargo.toml (si
//! se sube la versión sin tocarla, este test lo dice).

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn instalador_sin_go_y_con_la_version_del_cargo_toml() {
    let sh = repo_file("install.sh");
    let words = |s: &str| s.split(|c: char| !c.is_alphanumeric()).any(|w| w == "Go" || w == "golang");
    assert!(!words(&sh), "install.sh menciona Go");
    let want = format!("DEFAULT_VERSION=\"v{}\"", env!("CARGO_PKG_VERSION"));
    assert!(sh.contains(&want), "install.sh: la versión por defecto debe ser {want}");
    // Una sola constante: ninguna otra versión fija en el script.
    let fixed = sh.matches("\"v0.").count() + sh.matches(":-v0.").count();
    assert_eq!(fixed, 1, "install.sh tiene versiones fijas fuera de DEFAULT_VERSION");
    for readme in ["README.md", "README.es.md"] {
        let r = repo_file(readme);
        assert!(!r.contains("no Go") && !r.contains("v0.1.0"), "{readme}: restos del binario Go o versión vieja");
    }
}

//! S1 · Historia de símbolos barata (sin AST). Extrae, para cada commit, qué
//! FUNCIÓN tocó cada hunk usando el nombre de función que git ya emite en las
//! cabeceras de hunk `@@ -a,b +c,d @@ <funcname>`, pero con regex `xfuncname`
//! PROPIAS inyectadas (`-c core.attributesFile=… -c diff.<lang>.xfuncname=…`)
//! para no depender de la versión de git del usuario. Ver
//! `docs/DESIGN-GENERAL-CORE.md §9 (S1)` y `DECISIONS §4`.
//!
//! Este módulo NO toca el cursor de ingesta de `chrono-source-git` (la paridad
//! de git es sagrada): es un paso de enriquecimiento aparte que corre solo con
//! `init --symbols`, keyeado por el `sha` de commits ya ingeridos.
//!
//! ## Cómo funciona
//! 1. Escribimos un fichero de atributos temporal que mapea extensiones a
//!    "diff drivers" (p.ej. `*.rs diff=rust`).
//! 2. Lanzamos `git log --no-merges -U0 -p --no-color --format=<RS>%H` (más el
//!    rango, si lo hay) inyectando `-c core.attributesFile=…` y, por cada
//!    lenguaje, `-c diff.<lang>.xfuncname=<regex>`. La regex ya la escribe
//!    este módulo (no depende del git del usuario ni de sus builtins).
//! 3. Leemos el stdout del proceso LÍNEA A LÍNEA (RAM constante) y vamos
//!    trackeando fichero/hunk actuales; al cruzar el separador de registro
//!    (`\x1e` + sha de 40 hex) cerramos el commit anterior y llamamos a
//!    `on_commit`.
//!
//! ## Detalle de las regex `xfuncname`
//! Importante (y nada obvio): el mecanismo de git usa el **primer grupo de
//! captura que aparece en el patrón** (por posición del paréntesis de
//! apertura, no por cuál "gana" en el match) como el texto de `funcname`. Por
//! eso cada regex de abajo envuelve TODA la alternativa relevante en un único
//! grupo exterior que es, literalmente, el primer paréntesis del patrón — así
//! el funcname mostrado es la línea completa que matcheó ("pub fn foo", "def
//! bar", "class Baz"), no solo la palabra clave. Además, en este git (y en
//! git en general) NO se puede usar `(?:...)` para grupos no-capturantes en
//! POSIX ERE: probarlo produce `fatal: Invalid regexp`. Así que estas regex
//! solo usan grupos capturantes normales, con cuidado de que el primero sea
//! siempre el que queremos.
//!
//! Nota sobre precisión: para lenguajes con bloques anidados por llaves
//! (java/cfamily/ts con clases), el propio algoritmo de git para resolver el
//! `funcname` de un hunk (búsqueda hacia atrás sensible a indentación) puede
//! devolver el contenedor externo (p.ej. la clase) en vez del método interno
//! cuando el cambio está más anidado. Esto es un comportamiento del propio
//! git (no de nuestra regex) y es exactamente el motivo por el que S1 se
//! documenta como aproximado.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Separador de registro poco común usado para delimitar commits en la salida
/// de `git log --format=<RS>%H`. No aparece en shas ni en diffs de texto.
const REC_SEP: char = '\u{1e}';

/// Un símbolo (función/tipo/test) tocado por un commit en un fichero.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolTouch {
    /// Ruta del fichero (clave jerárquica con `/`), p.ej. "src/digraph.rs".
    pub file: String,
    /// Nombre del símbolo tal como lo emite git en la cabecera de hunk.
    pub symbol: String,
    pub added: i64,
    pub deleted: i64,
    /// Clase best-effort del símbolo: "fn" | "type" | "test" | "" (desconocido).
    pub sym_kind: String,
}

impl SymbolTouch {
    /// Clave de entidad del símbolo: `file#symbol` (separador `#`, NO `/`, para
    /// que el `LIKE 'dir/%'` de owners/hotspots no lo cuente como fichero).
    pub fn entity_key(&self) -> String {
        format!("{}#{}", self.file, self.symbol)
    }
}

/// Pares extensión→driver de diff, en orden fijo (definen el `.gitattributes`
/// temporal Y forman parte de `rules_hash`).
const EXT_LANG: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("go", "go"),
    ("py", "python"),
    ("ts", "ts"),
    ("tsx", "ts"),
    ("js", "js"),
    ("jsx", "js"),
    ("c", "cfamily"),
    ("h", "cfamily"),
    ("cc", "cfamily"),
    ("cpp", "cfamily"),
    ("hpp", "cfamily"),
    ("zig", "zig"),
    ("java", "java"),
];

// Regex `xfuncname` (POSIX ERE, sintaxis de `git help attributes` →
// "Defining a custom hunk-header"). Cada una envuelve la alternativa completa
// en su primer grupo capturante (ver comentario de módulo). Aproximadas a
// propósito: cubren los casos comunes, no un parser completo del lenguaje.

/// Rust: `fn`/`struct`/`enum`/`trait`/`mod` (con `pub`/`async`/`unsafe`
/// opcionales), `impl` (con genéricos opcionales) y atributos `#[..test..]`
/// (para que un cambio solo en el atributo de un test también matchee).
const RUST_XFUNCNAME: &str = r"^[[:space:]]*((pub[[:space:]]*(\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?(unsafe[[:space:]]+)?(fn|struct|enum|trait|mod)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|(pub[[:space:]]+)?impl([[:space:]]*<[^>]*>)?[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|#\[[A-Za-z0-9_:]*test[A-Za-z0-9_:]*\])";

/// Go: `func` (con o sin receiver) y `type`.
const GO_XFUNCNAME: &str = r"^((func([[:space:]]*\([^)]*\))?[[:space:]]+[A-Za-z_][A-Za-z0-9_]*)|(type[[:space:]]+[A-Za-z_][A-Za-z0-9_]*))";

/// Python: `def`/`async def` y `class`.
const PYTHON_XFUNCNAME: &str = r"^[[:space:]]*(((async[[:space:]]+)?def[[:space:]]+[A-Za-z_][A-Za-z0-9_]*)|(class[[:space:]]+[A-Za-z_][A-Za-z0-9_]*))";

/// TypeScript/JavaScript: `function` (con `export`/`default`/`async`
/// opcionales), `class`, y `const/let/var NOMBRE = (...) =>` (arrow function
/// asignada). Deliberadamente NO intenta reconocer métodos de clase sueltos
/// (`nombre(...) {`) porque en ERE, sin lookahead, esa forma también
/// matchearía `if (...) {` / `for (...) {` / `while (...) {`.
const JS_TS_XFUNCNAME: &str = r"^[[:space:]]*((export[[:space:]]+)?(default[[:space:]]+)?(abstract[[:space:]]+)?(async[[:space:]]+)?(function\*?[[:space:]]+[A-Za-z_$][A-Za-z0-9_$]*|class[[:space:]]+[A-Za-z_$][A-Za-z0-9_$]*|(const|let|var)[[:space:]]+[A-Za-z_$][A-Za-z0-9_$]*[[:space:]]*=[[:space:]]*(async[[:space:]]*)?\([^)]*\)[[:space:]]*=>))";

/// C/C++ (incluye headers): definición de función a nivel de fichero
/// (`tipo nombre(args) {`) y `struct`/`class`/`enum`/`union` (con `typedef`
/// opcional).
const CFAMILY_XFUNCNAME: &str = r"^((typedef[[:space:]]+)?(struct|class|enum|union)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_:<>,\*&[:space:]]*[[:space:]\*&][A-Za-z_][A-Za-z0-9_]*[[:space:]]*\([^;{}]*\)[[:space:]]*\{?[[:space:]]*$)";

/// Zig: `fn` (con `pub`/`export`/`extern` opcionales) y `const NOMBRE = struct`.
const ZIG_XFUNCNAME: &str = r"^((pub[[:space:]]+)?(export[[:space:]]+)?(extern[[:space:]]+)?fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|(pub[[:space:]]+)?const[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*(extern[[:space:]]+)?struct)";

/// Java: `class`, `interface`/`enum`, y firmas de método/constructor con
/// modificadores (`public`/`private`/`protected`/`static`/`final`/
/// `abstract`/`synchronized`).
const JAVA_XFUNCNAME: &str = r"^[[:space:]]*((public|private|protected)?[[:space:]]*(static[[:space:]]+)?(abstract[[:space:]]+)?(final[[:space:]]+)?class[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|(public[[:space:]]+)?(interface|enum)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|(public|private|protected|static|final|abstract|synchronized)([[:space:]]+(public|private|protected|static|final|abstract|synchronized))*[[:space:]]+[A-Za-z_][A-Za-z0-9_<>,\*&[:space:]]*[[:space:]\*&][A-Za-z_][A-Za-z0-9_]*[[:space:]]*\([^;{}]*\))";

/// Pares lenguaje(driver)→regex `xfuncname`, en orden fijo (uno por driver
/// usado en `EXT_LANG`; `ts` y `js` comparten texto de regex a propósito).
const LANG_XFUNCNAME: &[(&str, &str)] = &[
    ("rust", RUST_XFUNCNAME),
    ("go", GO_XFUNCNAME),
    ("python", PYTHON_XFUNCNAME),
    ("ts", JS_TS_XFUNCNAME),
    ("js", JS_TS_XFUNCNAME),
    ("cfamily", CFAMILY_XFUNCNAME),
    ("zig", ZIG_XFUNCNAME),
    ("java", JAVA_XFUNCNAME),
];

/// FNV-1a de 64 bits, determinista, sin dependencias externas.
fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hash determinista de las reglas `xfuncname` activas, para el manifiesto
/// (`symbols_rules_hash`): si cambian las regex, cambia el hash y hay que
/// reindexar los touches de símbolo.
pub fn rules_hash() -> String {
    let mut buf = String::new();
    for (ext, lang) in EXT_LANG {
        buf.push_str(ext);
        buf.push('=');
        buf.push_str(lang);
        buf.push(';');
    }
    for (lang, re) in LANG_XFUNCNAME {
        buf.push_str(lang);
        buf.push(':');
        buf.push_str(re);
        buf.push(';');
    }
    format!("{:016x}", fnv1a64(&buf))
}

/// Fichero de atributos temporal (RAII: se borra al hacer `drop`), único por
/// pid+contador para no chocar con otra instancia concurrente.
struct TempAttrsFile {
    path: PathBuf,
}

impl TempAttrsFile {
    fn create() -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "chrono-symbols-attrs-{}-{}.txt",
            std::process::id(),
            n
        ));
        let mut content = String::new();
        for (ext, lang) in EXT_LANG {
            content.push_str(&format!("*.{ext} diff={lang}\n"));
        }
        std::fs::write(&path, content)?;
        Ok(Self { path })
    }
}

impl Drop for TempAttrsFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Estado acumulado de UN commit mientras se recorre su diff línea a línea.
#[derive(Default)]
struct CommitState {
    sha: Option<String>,
    /// Fichero "nuevo" (`+++ b/<path>`) del hunk que se está leyendo. `None`
    /// si el fichero fue borrado (`+++ /dev/null`) o es binario: sus hunks se
    /// ignoran (ya cuentan como touch de fichero en la ingesta normal).
    current_file: Option<String>,
    /// `(file, funcname)` del hunk activo, o `None` si el hunk actual no trae
    /// funcname (cambio a nivel de fichero, no produce símbolo).
    current_key: Option<(String, String)>,
    /// Acumulado `(file, symbol) -> (added, deleted)`. `BTreeMap` para orden
    /// determinista (por `file` y luego `symbol`) sin ordenar a mano.
    counts: BTreeMap<(String, String), (i64, i64)>,
}

/// `true` si la extensión del fichero tiene un driver de diff configurado en
/// [`EXT_LANG`] (los únicos ficheros de los que extraemos símbolos; para el
/// resto git usaría su xfuncname por defecto y daría ruido).
fn is_known_ext(file: &str) -> bool {
    match file.rsplit('.').next() {
        Some(ext) if ext != file => EXT_LANG.iter().any(|(e, _)| *e == ext),
        _ => false,
    }
}

impl CommitState {
    /// Procesa una línea del diff (sin el salto de línea final) del commit
    /// actual, actualizando fichero/hunk activo o acumulando +/- líneas.
    fn feed_line(&mut self, line: &str) {
        if line.starts_with("diff --git ") {
            self.current_file = None;
            self.current_key = None;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.trim();
            self.current_file = if path == "/dev/null" {
                // Fichero borrado: ignoramos sus hunks (opción permitida por
                // el encargo; ya cuenta como touch de fichero normal).
                None
            } else {
                let file = path.strip_prefix("b/").unwrap_or(path).to_string();
                // Solo ficheros con una extensión de código con driver propio:
                // para el resto (Cargo.lock, .yml, .pyi…) git aplica su
                // xfuncname POR DEFECTO y emitiría "funcnames" basura (líneas
                // como `source = "..."` o `jobs:`). Restringir aquí evita ese
                // ruido en `hotspots --by symbol`.
                if is_known_ext(&file) {
                    Some(file)
                } else {
                    None
                }
            };
            self.current_key = None;
        } else if line.starts_with("Binary files ") {
            // Diff binario: sin hunks de texto que procesar.
            self.current_file = None;
            self.current_key = None;
        } else if line.starts_with("@@ ") {
            self.current_key = None;
            if let Some(file) = &self.current_file {
                if let Some(func) = parse_hunk_funcname(line) {
                    self.current_key = Some((file.clone(), func));
                }
            }
        } else if let Some(key) = self.current_key.clone() {
            // Líneas del cuerpo del hunk: solo contamos +/- de contenido, no
            // las cabeceras "+++"/"---" (ya tratadas arriba) ni el contexto.
            if line.starts_with('+') && !line.starts_with("+++") {
                self.counts.entry(key).or_insert((0, 0)).0 += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                self.counts.entry(key).or_insert((0, 0)).1 += 1;
            }
        }
        // Otras líneas ("--- a/…", texto del mensaje de commit, "\ No newline
        // at end of file", etc.) no aportan nada y se ignoran.
    }

    /// Cierra el commit: vuelca lo acumulado a `Vec<SymbolTouch>`, YA
    /// ordenado por `(file, symbol)` (orden natural de `BTreeMap`).
    fn finish(self) -> Vec<SymbolTouch> {
        self.counts
            .into_iter()
            .map(|((file, symbol), (added, deleted))| {
                let sym_kind = classify_kind(&symbol);
                SymbolTouch { file, symbol, added, deleted, sym_kind }
            })
            .collect()
    }
}

/// Extrae el `<funcname>` de una cabecera de hunk `@@ -a,b +c,d @@ <func>`.
/// Devuelve `None` si no hay cabecera reconocible o el funcname viene vacío.
fn parse_hunk_funcname(line: &str) -> Option<String> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let func = rest[end + 3..].trim();
    if func.is_empty() {
        None
    } else {
        Some(func.to_string())
    }
}

/// Clase best-effort de un símbolo a partir del texto de `funcname` que emite
/// git. No es crítico (campo informativo): "test" si el nombre sugiere un
/// test, "type" si sugiere una definición de tipo, si no "fn".
fn classify_kind(symbol: &str) -> String {
    let lower = symbol.to_ascii_lowercase();
    if lower.contains("test") {
        return "test".to_string();
    }
    const TYPE_KEYWORDS: [&str; 6] = ["struct", "enum", "class", "trait", "interface", "impl"];
    if TYPE_KEYWORDS.iter().any(|k| lower.contains(k)) || lower.starts_with("type ") {
        return "type".to_string();
    }
    "fn".to_string()
}

/// Streamea `git log --no-merges -p -U0` sobre el rango (`since_sha..HEAD`, o
/// todo el historial si `since_sha` es `None`), inyectando las regex
/// `xfuncname` propias (vía un fichero de atributos temporal que gestiona esta
/// función), y llama a `on_commit(sha, symbols)` una vez por commit con los
/// símbolos que tocó, ya agregados por `file#symbol`. RAM constante (un commit
/// a la vez). Si `on_commit` devuelve `Err`, se aborta y se propaga.
pub fn stream_symbols(
    repo: &Path,
    since_sha: Option<&str>,
    on_commit: &mut dyn FnMut(&str, Vec<SymbolTouch>) -> Result<()>,
) -> Result<()> {
    let attrs = TempAttrsFile::create()?;

    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo);
    cmd.arg("-c").arg(format!("core.attributesFile={}", attrs.path.display()));
    for (lang, re) in LANG_XFUNCNAME {
        cmd.arg("-c").arg(format!("diff.{lang}.xfuncname={re}"));
    }
    cmd.args(["log", "--no-merges", "-U0", "-p", "--no-color"]);
    cmd.arg(format!("--format={REC_SEP}%H"));
    if let Some(sha) = since_sha {
        cmd.arg(format!("{sha}..HEAD"));
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().ok_or("no se pudo abrir stdout de git log")?;

    // Drenamos stderr en un hilo aparte para no arriesgarnos a un deadlock si
    // git escribe más de lo que cabe en el buffer del pipe mientras nosotros
    // seguimos leyendo stdout.
    let mut stderr_pipe = child.stderr.take().ok_or("no se pudo abrir stderr de git log")?;
    let stderr_thread = std::thread::spawn(move || -> String {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let mut br = BufReader::with_capacity(1 << 20, stdout);
    let mut state = CommitState::default();
    let mut raw = Vec::new();
    let mut callback_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;

    loop {
        raw.clear();
        let n = br.read_until(b'\n', &mut raw)?;
        if n == 0 {
            break;
        }
        while matches!(raw.last(), Some(b'\n') | Some(b'\r')) {
            raw.pop();
        }
        // `from_utf8_lossy` en vez de asumir UTF-8 estricto: mensajes de
        // commit o nombres de fichero con bytes no-UTF8 no deben abortar el
        // streaming (igual que el resto del crate, que usa `lossy` al leer
        // salida de git).
        let line = String::from_utf8_lossy(&raw);

        if let Some(sha) = line.strip_prefix(REC_SEP) {
            if let Some(prev_sha) = state.sha.take() {
                let touches = std::mem::take(&mut state).finish();
                if let Err(e) = on_commit(&prev_sha, touches) {
                    callback_err = Some(e);
                    break;
                }
            }
            state = CommitState { sha: Some(sha.trim().to_string()), ..Default::default() };
            continue;
        }
        state.feed_line(&line);
    }

    if let Some(e) = callback_err {
        // `on_commit` abortó: no drenamos más, matamos el proceso para que no
        // se quede bloqueado escribiendo a un pipe que ya no leemos.
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_thread.join();
        return Err(e);
    }

    // Último commit del stream (no hay separador final que lo cierre).
    if let Some(prev_sha) = state.sha.take() {
        let touches = std::mem::take(&mut state).finish();
        on_commit(&prev_sha, touches)?;
    }

    let status = child.wait()?;
    let stderr_text = stderr_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("git log falló: {}", stderr_text.trim()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconstruye, a partir de texto plano (SIN ejecutar git), los pares
    /// `(sha, símbolos)` que produciría `stream_symbols` — mismo estado y
    /// mismas reglas de parseo, alimentadas línea a línea desde un `&str`
    /// en vez de un proceso hijo. Existe solo para testear el parser puro.
    fn parse_commits(text: &str) -> Vec<(String, Vec<SymbolTouch>)> {
        let mut out = Vec::new();
        let mut state = CommitState::default();
        for line in text.lines() {
            if let Some(sha) = line.strip_prefix(REC_SEP) {
                if let Some(prev_sha) = state.sha.take() {
                    out.push((prev_sha, std::mem::take(&mut state).finish()));
                }
                state = CommitState { sha: Some(sha.trim().to_string()), ..Default::default() };
                continue;
            }
            state.feed_line(line);
        }
        if let Some(prev_sha) = state.sha.take() {
            out.push((prev_sha, state.finish()));
        }
        out
    }

    fn touch(file: &str, symbol: &str, added: i64, deleted: i64, kind: &str) -> SymbolTouch {
        SymbolTouch {
            file: file.to_string(),
            symbol: symbol.to_string(),
            added,
            deleted,
            sym_kind: kind.to_string(),
        }
    }

    #[test]
    fn parsea_hunks_con_y_sin_funcname_en_varios_ficheros_y_commits() {
        let sha_a = "a".repeat(40);
        let sha_b = "b".repeat(40);
        let text = format!(
            "{sep}{sha_a}\n\
             algún mensaje de commit\n\
             \n\
             diff --git a/src/lib.rs b/src/lib.rs\n\
             index 111..222 100644\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -10,2 +10,3 @@ fn foo\n\
             -vieja linea 1\n\
             -vieja linea 2\n\
             +nueva linea 1\n\
             +nueva linea 2\n\
             +nueva linea 3\n\
             @@ -20,1 +21,1 @@\n\
             -sin funcname antes\n\
             +sin funcname despues\n\
             diff --git a/src/util.py b/src/util.py\n\
             index 333..444 100644\n\
             --- a/src/util.py\n\
             +++ b/src/util.py\n\
             @@ -1,0 +2,1 @@ def bar\n\
             +linea añadida\n\
             {sep}{sha_b}\n\
             otro mensaje\n\
             diff --git a/src/lib.rs b/src/lib.rs\n\
             index 222..333 100644\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -5,1 +5,1 @@ fn foo\n\
             -x\n\
             +y\n",
            sep = REC_SEP,
        );

        let commits = parse_commits(&text);
        assert_eq!(commits.len(), 2);

        let (sha0, syms0) = &commits[0];
        assert_eq!(sha0, &sha_a);
        // El hunk sin funcname NO debe producir un símbolo aparte: solo dos
        // entradas, ordenadas por (file, symbol).
        assert_eq!(
            syms0,
            &vec![
                touch("src/lib.rs", "fn foo", 3, 2, "fn"),
                touch("src/util.py", "def bar", 1, 0, "fn"),
            ]
        );

        let (sha1, syms1) = &commits[1];
        assert_eq!(sha1, &sha_b);
        assert_eq!(syms1, &vec![touch("src/lib.rs", "fn foo", 1, 1, "fn")]);
    }

    #[test]
    fn ignora_hunks_de_ficheros_binarios_y_borrados() {
        let sha = "c".repeat(40);
        let text = format!(
            "{sep}{sha}\n\
             diff --git a/img.png b/img.png\n\
             index 111..222 100644\n\
             Binary files a/img.png and b/img.png differ\n\
             diff --git a/old.rs b/old.rs\n\
             deleted file mode 100644\n\
             index 111..0000000\n\
             --- a/old.rs\n\
             +++ /dev/null\n\
             @@ -1,1 +0,0 @@ fn viejo\n\
             -fn viejo() {{}}\n",
            sep = REC_SEP,
        );
        let commits = parse_commits(&text);
        assert_eq!(commits.len(), 1);
        assert!(commits[0].1.is_empty());
    }

    #[test]
    fn clasifica_sym_kind_best_effort() {
        assert_eq!(classify_kind("fn test_foo"), "test");
        assert_eq!(classify_kind("#[test]"), "test");
        assert_eq!(classify_kind("pub struct Foo"), "type");
        assert_eq!(classify_kind("class Bar"), "type");
        assert_eq!(classify_kind("impl Foo"), "type");
        assert_eq!(classify_kind("pub fn foo"), "fn");
        assert_eq!(classify_kind("type Bar"), "type");
    }

    #[test]
    fn rules_hash_es_determinista_y_no_vacio() {
        let h1 = rules_hash();
        let h2 = rules_hash();
        assert!(!h1.is_empty());
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16); // hex de u64
    }

    fn init_repo(dir: &Path) {
        Command::new("git").arg("init").arg("-q").arg(dir).status().unwrap();
        Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "config", "user.email", "a@b.c"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "config", "user.name", "Ana"])
            .status()
            .unwrap();
    }

    #[test]
    fn end_to_end_con_repo_git_temporal() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("git no disponible: se salta el test end-to-end");
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "chrono-cli-symbols-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        std::fs::write(
            dir.join("a.rs"),
            "pub fn foo(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        Command::new("git").args(["-C", dir.to_str().unwrap(), "add", "."]).status().unwrap();
        Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        std::fs::write(
            dir.join("a.rs"),
            "pub fn foo(x: i32) -> i32 {\n    x + 2\n}\n",
        )
        .unwrap();
        Command::new("git").args(["-C", dir.to_str().unwrap(), "add", "."]).status().unwrap();
        Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "commit", "-q", "-m", "cambia foo"])
            .status()
            .unwrap();

        let mut seen: Vec<(String, Vec<SymbolTouch>)> = Vec::new();
        stream_symbols(&dir, None, &mut |sha, syms| {
            seen.push((sha.to_string(), syms));
            Ok(())
        })
        .unwrap();

        assert_eq!(seen.len(), 2);
        // `git log` (y por tanto stream_symbols) entrega los commits del más
        // nuevo al más viejo: seen[0] es "cambia foo" (toca una línea dentro
        // de `foo`, sí debe producir símbolo). seen[1] es "init": crea el
        // fichero entero, con -U0 el hunk es "fichero nuevo completo" y no
        // trae funcname (no hay línea previa hacia atrás que matchee), así
        // que no produce símbolos — comportamiento esperado, no un fallo.
        let (_, syms_cambia) = &seen[0];
        assert_eq!(syms_cambia.len(), 1);
        assert_eq!(syms_cambia[0].file, "a.rs");
        assert!(syms_cambia[0].symbol.contains("foo"));
        assert_eq!(syms_cambia[0].added, 1);
        assert_eq!(syms_cambia[0].deleted, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

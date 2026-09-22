//! Partición de una línea CSV en campos, a mano (sin la crate `csv`), estilo
//! RFC4180: campos entrecomillados con `"`, comillas dobles `""` dentro de un
//! campo entrecomillado escapan una comilla literal, y el delimitador dentro
//! de comillas no separa campos.
//!
//! LIMITACIÓN DE v1 (documentada en el encargo): no se soportan saltos de
//! línea dentro de un campo entrecomillado. El formato es "un registro por
//! línea física": esto es lo que permite que el watermark incremental sea un
//! simple offset en bytes (`cursor::CsvCursor`), sin tener que reconstruir
//! registros multi-línea para saber dónde retomar la lectura.

/// Parte `line` (una línea física, sin salto final) en campos según
/// `delimiter`. No hace trim de espacios: un campo `" b"` fuera de comillas
/// conserva el espacio tal cual (igual que la mayoría de lectores CSV).
pub fn parse_csv_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"'); // `""` dentro de comillas: comilla literal escapada.
                    chars.next();
                } else {
                    in_quotes = false; // cierre de campo entrecomillado.
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() {
            in_quotes = true; // apertura de campo entrecomillado (solo al inicio del campo).
        } else if c == delimiter {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separa_por_coma_simple() {
        assert_eq!(parse_csv_line("a,b,c", ','), vec!["a", "b", "c"]);
    }

    #[test]
    fn campo_entrecomillado_con_coma_no_se_parte() {
        assert_eq!(parse_csv_line(r#"a,"b, c",d"#, ','), vec!["a", "b, c", "d"]);
    }

    #[test]
    fn comillas_dobles_escapan_una_comilla_literal() {
        assert_eq!(parse_csv_line(r#""el ""mejor"" caso",b"#, ','), vec![r#"el "mejor" caso"#, "b"]);
    }

    #[test]
    fn tsv_usa_tab_como_delimitador() {
        assert_eq!(parse_csv_line("a\tb\tc", '\t'), vec!["a", "b", "c"]);
    }

    #[test]
    fn campo_vacio_al_final_se_conserva() {
        assert_eq!(parse_csv_line("a,b,", ','), vec!["a", "b", ""]);
    }

    #[test]
    fn linea_vacia_es_un_campo_vacio() {
        assert_eq!(parse_csv_line("", ','), vec![""]);
    }
}

//! Puerto Classifier: Level-0 (reglas) y Level-1 (un modelo Chispa entrenado
//! con `kling ai chispa train`, consultado desde fuera) implementan el mismo
//! trait y se encadenan con precedencia explícita. Las señales duras ganan
//! siempre; Chispa solo actúa cuando su predicción es `confident`.

use crate::domain::{Event, Label};

/// Un clasificador produce cero o más etiquetas (varias tareas) para un evento.
pub trait Classifier {
    fn classify(&self, ev: &Event) -> Vec<Label>;
    fn name(&self) -> &str;
}

/// Cadena determinista de clasificadores. Para cada `task`, la PRIMERA etiqueta
/// producida gana: coloca primero el de señales duras (reglas), luego Chispa.
pub struct Chain {
    stages: Vec<Box<dyn Classifier>>,
}

impl Chain {
    pub fn new(stages: Vec<Box<dyn Classifier>>) -> Self {
        Self { stages }
    }

    pub fn classify(&self, ev: &Event) -> Vec<Label> {
        let mut out: Vec<Label> = Vec::new();
        for stage in &self.stages {
            for label in stage.classify(ev) {
                if !out.iter().any(|l| l.task == label.task) {
                    out.push(label);
                }
            }
        }
        out.sort_by(|a, b| a.task.cmp(&b.task));
        out
    }
}

impl Classifier for Chain {
    fn classify(&self, ev: &Event) -> Vec<Label> {
        Chain::classify(self, ev)
    }
    fn name(&self) -> &str {
        "chain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, Vec<Label>);
    impl Classifier for Fixed {
        fn classify(&self, _ev: &Event) -> Vec<Label> {
            self.1.clone()
        }
        fn name(&self) -> &str {
            self.0
        }
    }

    fn lbl(task: &str, label: &str, source: &str) -> Label {
        Label { task: task.into(), label: label.into(), confidence: 1.0, source: source.into(), evidence: vec![] }
    }

    #[test]
    fn la_primera_etapa_gana_por_tarea() {
        let rules = Box::new(Fixed("rules", vec![lbl("kind", "fix", "rules")]));
        let chispa = Box::new(Fixed("chispa", vec![lbl("kind", "feat", "chispa"), lbl("bug_category", "network", "chispa")]));
        let chain = Chain::new(vec![rules, chispa]);
        let out = chain.classify(&Event::default());
        // "kind" lo fija reglas (fix), no Chispa (feat); "bug_category" lo aporta Chispa.
        assert_eq!(out.len(), 2);
        let kind = out.iter().find(|l| l.task == "kind").unwrap();
        assert_eq!(kind.label, "fix");
        assert_eq!(kind.source, "rules");
        assert_eq!(out.iter().find(|l| l.task == "bug_category").unwrap().label, "network");
    }
}

use std::path::Path;

use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;

pub(crate) fn build_env(template_dir: &Path) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader(template_dir));
    env
}

pub(crate) fn snapshot_context(snapshot: &RuntimeSnapshot) -> minijinja::Value {
    minijinja::Value::from_serialize(snapshot)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn embedded_templates_parse() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        for name in [
            "index.html",
            "movements.html",
            "triggers.html",
            "agents.html",
            "tasks.html",
            "logs.html",
            "layout.html",
        ] {
            env.get_template(name)
                .unwrap_or_else(|e| panic!("template {name} failed to parse: {e}"));
        }
    }
}

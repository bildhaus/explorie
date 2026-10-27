use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, Contribution, Detail, Inspection, Manifest, Plugin, run_stdio,
};
use serde_json::Value;

struct Example;

impl Plugin for Example {
    fn manifest(&self) -> Manifest {
        serde_json::from_str(include_str!("../plugin.json")).expect("valid example manifest")
    }

    fn configure(&mut self, _: Value) -> Result<(), String> {
        Ok(())
    }

    fn inspect(&mut self, context: Inspection) -> Result<Contribution, String> {
        let mut contribution = Contribution::empty(&context);
        contribution.badge = Some("Example".into());
        contribution.details.push(Detail {
            label: "Entries in current listing".into(),
            value: context.entries.len().to_string(),
        });
        Ok(contribution)
    }

    fn invoke(&mut self, _: ActionRequest) -> Result<ActionEffect, String> {
        Ok(ActionEffect::None)
    }
}

fn main() -> std::io::Result<()> {
    run_stdio(Example)
}

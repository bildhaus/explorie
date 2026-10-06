use std::path::{Path, PathBuf};

use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, Contribution, Detail, Inspection, Manifest, Plugin, PluginAction,
};
use serde_json::Value;

#[derive(Default)]
pub struct ObsidianPlugin {
    custom_root: Option<PathBuf>,
}

fn vault_root(path: &Path, custom: Option<&Path>) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".obsidian").is_dir() || custom == Some(*ancestor))
        .map(Path::to_path_buf)
}

fn uri(path: &Path) -> String {
    let encoded: String =
        url::form_urlencoded::byte_serialize(path.to_string_lossy().as_bytes()).collect();
    format!("obsidian://open?path={}", encoded.replace('+', "%20"))
}

fn vault_uri(root: &Path) -> Result<String, String> {
    let name = root.file_name().ok_or("This vault has no folder name")?;
    let encoded: String =
        url::form_urlencoded::byte_serialize(name.to_string_lossy().as_bytes()).collect();
    Ok(format!(
        "obsidian://open?vault={}",
        encoded.replace('+', "%20")
    ))
}

impl Plugin for ObsidianPlugin {
    fn manifest(&self) -> Manifest {
        serde_json::from_str(include_str!("../plugin.json")).expect("valid bundled manifest")
    }
    fn configure(&mut self, value: Value) -> Result<(), String> {
        let root = value.get("vaultRoot").and_then(Value::as_str).unwrap_or("");
        self.custom_root = if root.is_empty() {
            None
        } else {
            let path = PathBuf::from(root);
            if !path.is_absolute() || !path.is_dir() {
                return Err("Choose an existing absolute vault directory".into());
            }
            Some(path)
        };
        Ok(())
    }
    fn inspect(&mut self, context: Inspection) -> Result<Contribution, String> {
        let mut result = Contribution::empty(&context);
        let Some(root) = vault_root(&context.path, self.custom_root.as_deref()) else {
            return Ok(result);
        };
        result.badge = Some("Obsidian".into());
        result.root = Some(root.clone());
        result.details.push(Detail {
            label: "Vault".into(),
            value: root.display().to_string(),
        });
        result.actions = vec![
            PluginAction {
                id: "open-vault".into(),
                label: "Open Vault".into(),
            },
            PluginAction {
                id: "copy-link".into(),
                label: "Copy Obsidian Link".into(),
            },
        ];
        if selected_note(&context, &root).is_some() {
            result.actions.push(PluginAction {
                id: "open-note".into(),
                label: "Open Note".into(),
            });
        }
        Ok(result)
    }
    fn invoke(&mut self, request: ActionRequest) -> Result<ActionEffect, String> {
        let root = vault_root(&request.context.path, self.custom_root.as_deref())
            .ok_or("This folder is not in an Obsidian vault")?;
        let note = selected_note(&request.context, &root);
        match request.action_id.as_str() {
            "open-vault" => Ok(ActionEffect::OpenUrl(vault_uri(&root)?)),
            "open-note" => note
                .map(|path| ActionEffect::OpenUrl(uri(path)))
                .ok_or("Select a Markdown note in this vault".into()),
            "copy-link" => Ok(ActionEffect::CopyText(match note {
                Some(note) => uri(note),
                None => vault_uri(&root)?,
            })),
            _ => Err("Unknown Obsidian action".into()),
        }
    }
}

fn selected_note<'a>(context: &'a Inspection, root: &Path) -> Option<&'a Path> {
    if context.selected.len() != 1 {
        return None;
    }
    let path = &context.selected[0];
    (path.starts_with(root)
        && path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md")))
    .then_some(path.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn detects_nested_and_custom_vaults_without_writing_configuration() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".obsidian")).unwrap();
        let nested = temp.path().join("notes");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(vault_root(&nested, None).as_deref(), Some(temp.path()));
        let custom = tempfile::tempdir().unwrap();
        let mut plugin = ObsidianPlugin::default();
        plugin
            .configure(json!({"vaultRoot":custom.path()}))
            .unwrap();
        assert_eq!(
            vault_root(custom.path(), plugin.custom_root.as_deref()).as_deref(),
            Some(custom.path())
        );
    }
    #[test]
    fn uri_encodes_reserved_characters_and_unicode() {
        let value = uri(Path::new("/vault/שלום # &?.md"));
        assert!(value.contains("%23%20%26%3F.md"));
        assert!(!value.contains('+'));
        let parsed = url::Url::parse(&value).unwrap();
        assert_eq!(
            parsed.query_pairs().next().unwrap().1,
            "/vault/שלום # &?.md"
        );
    }
    #[test]
    fn only_offers_selected_notes_within_vault() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".obsidian")).unwrap();
        let note = temp.path().join("note.md");
        std::fs::write(&note, "test").unwrap();
        let context = Inspection {
            path: temp.path().into(),
            selected: vec![note],
            ..Default::default()
        };
        let mut plugin = ObsidianPlugin::default();
        assert!(
            plugin
                .inspect(context.clone())
                .unwrap()
                .actions
                .iter()
                .any(|a| a.id == "open-note")
        );
        assert!(matches!(
            plugin
                .invoke(ActionRequest {
                    action_id: "copy-link".into(),
                    context
                })
                .unwrap(),
            ActionEffect::CopyText(_)
        ));
    }
    #[test]
    fn vault_links_use_the_documented_vault_parameter() {
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("Notes # & שלום");
        std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let expected = "obsidian://open?vault=Notes%20%23%20%26%20%D7%A9%D7%9C%D7%95%D7%9D";
        assert_eq!(vault_uri(&vault).unwrap(), expected);
        let context = Inspection {
            path: vault,
            ..Inspection::default()
        };
        let mut plugin = ObsidianPlugin::default();
        assert!(matches!(plugin.invoke(ActionRequest {
            action_id: "open-vault".into(),
            context: context.clone(),
        }).unwrap(), ActionEffect::OpenUrl(value) if value == expected));
        assert!(matches!(plugin.invoke(ActionRequest {
            action_id: "copy-link".into(),
            context,
        }).unwrap(), ActionEffect::CopyText(value) if value == expected));
    }
}

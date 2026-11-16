use crate::config::Config;
use crate::providers::canonical::{maybe_get_canonical_model, CanonicalModel, Modality};
use crate::session::extension_data::ExtensionState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub const SUBAGENT_ROUTING_KEY: &str = "GOOSE_SUBAGENT_ROUTING";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Fast,
    Balanced,
    Powerful,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Fast, Tier::Balanced, Tier::Powerful];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Balanced => "balanced",
            Tier::Powerful => "powerful",
        }
    }

    fn parse(name: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|tier| tier.as_str() == name)
    }

    fn purpose(self) -> &'static str {
        match self {
            Tier::Fast => "lookups, one-line fixes",
            Tier::Balanced => "single-file edits, known bugs",
            Tier::Powerful => "multi-file refactors, architecture, security, long reasoning",
        }
    }

    fn nearest_first(self) -> [Tier; 3] {
        match self {
            Tier::Fast => [Tier::Fast, Tier::Balanced, Tier::Powerful],
            Tier::Balanced => [Tier::Balanced, Tier::Powerful, Tier::Fast],
            Tier::Powerful => [Tier::Powerful, Tier::Balanced, Tier::Fast],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    ToolCall,
    Vision,
    Reasoning,
    MinContext(usize),
}

impl Requirement {
    pub fn parse(value: &str) -> Option<Requirement> {
        match value.trim() {
            "tool_call" => Some(Requirement::ToolCall),
            "vision" => Some(Requirement::Vision),
            "reasoning" => Some(Requirement::Reasoning),
            other => other
                .strip_prefix("min_context:")
                .and_then(|tokens| tokens.trim().parse().ok())
                .map(Requirement::MinContext),
        }
    }

    /// `None` when the metadata does not say.
    fn met_by(self, model: &CanonicalModel) -> Option<bool> {
        match self {
            Requirement::ToolCall => Some(model.tool_call),
            Requirement::Vision => (!model.modalities.input.is_empty())
                .then(|| model.modalities.input.contains(&Modality::Image)),
            Requirement::Reasoning => model.reasoning,
            Requirement::MinContext(tokens) => {
                (model.limit.context > 0).then_some(model.limit.context >= tokens)
            }
        }
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Requirement::ToolCall => write!(f, "tool_call"),
            Requirement::Vision => write!(f, "vision"),
            Requirement::Reasoning => write!(f, "reasoning"),
            Requirement::MinContext(tokens) => write!(f, "min_context:{tokens}"),
        }
    }
}

/// The requirements a model fails according to canonical metadata. `tool_call` is always
/// checked; anything the metadata does not record counts as met.
pub fn unmet_requirements(
    provider: &str,
    model: &str,
    requires: &[Requirement],
) -> Vec<Requirement> {
    let Some(canonical) = maybe_get_canonical_model(provider, model) else {
        return Vec::new();
    };
    std::iter::once(Requirement::ToolCall)
        .chain(requires.iter().copied())
        .filter(|requirement| requirement.met_by(&canonical) == Some(false))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedModel {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default)]
pub struct Capability {
    pub name: String,
    pub description: Option<String>,
    pub requires: Vec<Requirement>,
    pub extensions: Vec<String>,
    pub instructions: Option<String>,
    pub tiers: Vec<(Tier, RoutedModel)>,
}

impl Capability {
    fn tier(&self, tier: Tier) -> Option<&RoutedModel> {
        self.tiers
            .iter()
            .find(|(configured, _)| *configured == tier)
            .map(|(_, model)| model)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RoutingTable {
    pub default_capability: Option<String>,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChoice {
    pub tier: Tier,
    pub model: RoutedModel,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub capability: Capability,
    pub requested_tier: Tier,
    pub choice: Option<RouteChoice>,
    pub notes: Vec<String>,
}

/// What the table chose for a subagent, kept on its session so the run-started update can
/// report it while the subagent still runs that model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRoutingState {
    pub capability: String,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub provider: String,
    pub model: String,
}

impl ExtensionState for SubagentRoutingState {
    const EXTENSION_NAME: &'static str = "subagent_routing";
    const VERSION: &'static str = "v0";
}

fn string_field(entry: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn string_list(
    entry: &serde_json::Map<String, Value>,
    key: &str,
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    match entry.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let text = item.as_str().map(str::trim).filter(|text| !text.is_empty());
                if text.is_none() {
                    warnings.push(format!("{context}: skipped a non-text entry in '{key}'"));
                }
                text.map(str::to_string)
            })
            .collect(),
        Some(_) => {
            warnings.push(format!("{context}: '{key}' must be a list"));
            Vec::new()
        }
    }
}

fn parse_capability(name: &str, value: &Value, warnings: &mut Vec<String>) -> Option<Capability> {
    let context = format!("{SUBAGENT_ROUTING_KEY} capability '{name}'");
    let Some(entry) = value.as_object() else {
        warnings.push(format!("{context}: skipped, it must be a mapping"));
        return None;
    };
    let requires = string_list(entry, "requires", &context, warnings)
        .into_iter()
        .filter_map(|text| {
            let requirement = Requirement::parse(&text);
            if requirement.is_none() {
                warnings.push(format!("{context}: unknown requirement '{text}' ignored"));
            }
            requirement
        })
        .collect();
    let mut tiers = Vec::new();
    match entry.get("tiers") {
        None | Some(Value::Null) => {}
        Some(Value::Object(configured)) => {
            for (tier_name, model) in configured {
                let Some(tier) = Tier::parse(tier_name) else {
                    warnings.push(format!("{context}: unknown tier '{tier_name}' skipped"));
                    continue;
                };
                let model = model.as_object().and_then(|model| {
                    Some(RoutedModel {
                        provider: string_field(model, "provider")?,
                        model: string_field(model, "model")?,
                    })
                });
                match model {
                    Some(model) => tiers.push((tier, model)),
                    None => warnings.push(format!(
                        "{context}: tier '{tier_name}' skipped, it needs a provider and a model"
                    )),
                }
            }
        }
        Some(_) => warnings.push(format!("{context}: 'tiers' must be a mapping")),
    }
    Some(Capability {
        name: name.to_string(),
        description: string_field(entry, "description"),
        requires,
        extensions: string_list(entry, "extensions", &context, warnings),
        instructions: string_field(entry, "instructions"),
        tiers,
    })
}

impl RoutingTable {
    /// Reads a possibly partial table. Entries that cannot be used are left out and described
    /// in the returned warnings. The table is `None` when no capability is usable.
    pub fn parse(value: &Value) -> (Option<RoutingTable>, Vec<String>) {
        let mut warnings = Vec::new();
        if value.is_null() {
            return (None, warnings);
        }
        let Some(root) = value.as_object() else {
            warnings.push(format!("{SUBAGENT_ROUTING_KEY} must be a mapping; ignored"));
            return (None, warnings);
        };
        let mut capabilities = Vec::new();
        match root.get("capabilities") {
            Some(Value::Object(configured)) => {
                for (name, value) in configured {
                    if name.trim().is_empty() {
                        warnings.push(format!(
                            "{SUBAGENT_ROUTING_KEY}: skipped a capability without a name"
                        ));
                    } else if let Some(capability) = parse_capability(name, value, &mut warnings) {
                        capabilities.push(capability);
                    }
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => warnings.push(format!(
                "{SUBAGENT_ROUTING_KEY}: 'capabilities' must be a mapping"
            )),
        }
        let default_capability = string_field(root, "default_capability").filter(|name| {
            let known = capabilities.iter().any(|capability| capability.name == *name);
            if !known {
                warnings.push(format!(
                    "{SUBAGENT_ROUTING_KEY}: default_capability '{name}' is not a configured capability"
                ));
            }
            known
        });
        if capabilities.is_empty() {
            return (None, warnings);
        }
        (
            Some(RoutingTable {
                default_capability,
                capabilities,
            }),
            warnings,
        )
    }

    pub fn load(config: &Config) -> (Option<RoutingTable>, Vec<String>) {
        match config.get_param::<Value>(SUBAGENT_ROUTING_KEY) {
            Ok(value) => Self::parse(&value),
            Err(crate::config::ConfigError::NotFound(_)) => (None, Vec::new()),
            Err(error) => (
                None,
                vec![format!("{SUBAGENT_ROUTING_KEY} could not be read: {error}")],
            ),
        }
    }

    pub fn capability(&self, name: &str) -> Option<&Capability> {
        self.capabilities
            .iter()
            .find(|capability| capability.name == name)
    }

    pub fn capability_names(&self) -> Vec<&str> {
        self.capabilities
            .iter()
            .map(|capability| capability.name.as_str())
            .collect()
    }

    /// Picks the model for a delegate. A missing or unsuitable tier falls back to the nearest
    /// tier of the same capability, then to the same tier of the default capability. `None`
    /// without an error when no capability was named and there is no default.
    pub fn route(
        &self,
        capability: Option<&str>,
        tier: Option<Tier>,
    ) -> Result<Option<Route>, String> {
        let Some(name) = capability.or(self.default_capability.as_deref()) else {
            return Ok(None);
        };
        let capability = self.capability(name).ok_or_else(|| {
            format!(
                "Unknown capability '{name}'; configured: {}",
                self.capability_names().join(", ")
            )
        })?;
        let requested_tier = tier.unwrap_or(Tier::Balanced);
        let fallback = self
            .default_capability
            .as_deref()
            .filter(|default| *default != capability.name)
            .and_then(|default| self.capability(default))
            .and_then(|default| {
                default
                    .tier(requested_tier)
                    .map(|model| (default.name.as_str(), requested_tier, model))
            });
        let candidates = requested_tier
            .nearest_first()
            .into_iter()
            .filter_map(|tier| {
                capability
                    .tier(tier)
                    .map(|model| (capability.name.as_str(), tier, model))
            })
            .chain(fallback);

        let mut notes = Vec::new();
        let mut choice = None;
        for (source, tier, model) in candidates {
            let unmet = unmet_requirements(&model.provider, &model.model, &capability.requires);
            if unmet.is_empty() {
                choice = Some(RouteChoice {
                    tier,
                    model: model.clone(),
                });
                if (source, tier) != (capability.name.as_str(), requested_tier) {
                    notes.push(format!(
                        "Routing: {}/{} is not usable; used {source}/{} ({}/{}).",
                        capability.name,
                        requested_tier.as_str(),
                        tier.as_str(),
                        model.provider,
                        model.model
                    ));
                }
                break;
            }
            notes.push(format!(
                "Routing: skipped {}/{} for {source}/{}: it lacks {}.",
                model.provider,
                model.model,
                tier.as_str(),
                unmet
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if choice.is_none() {
            notes.push(format!(
                "Routing: no configured model suits {}/{}; the subagent uses the default subagent model.",
                capability.name,
                requested_tier.as_str()
            ));
        }
        Ok(Some(Route {
            capability: capability.clone(),
            requested_tier,
            choice,
            notes,
        }))
    }

    pub fn describe(&self) -> String {
        let mut out = String::from(
            "\n\nModel routing: set `capability` and `tier` to choose the subagent's model from the user's routing table.\nCapabilities:",
        );
        for capability in &self.capabilities {
            out.push_str(&format!("\n- {}", capability.name));
            if let Some(description) = &capability.description {
                out.push_str(&format!(": {description}"));
            }
            if self.default_capability.as_deref() == Some(capability.name.as_str()) {
                out.push_str(" (default)");
            }
        }
        out.push_str("\nTiers:");
        for tier in Tier::ALL {
            out.push_str(&format!("\n- {}: {}", tier.as_str(), tier.purpose()));
            if tier == Tier::Balanced {
                out.push_str(" (used when `tier` is omitted)");
            }
        }
        if self.default_capability.is_none() {
            out.push_str("\nWithout `capability` the table is not used.");
        }
        out.push_str(
            "\nAn explicit `provider` or `model`, or a model set by the source recipe or agent, takes precedence over the table. Give a short `routing_reason` for your choice.",
        );
        out
    }
}

pub fn config_warnings() -> Vec<String> {
    RoutingTable::load(Config::global()).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(value: Value) -> RoutingTable {
        let (table, warnings) = RoutingTable::parse(&value);
        assert!(warnings.is_empty(), "{warnings:?}");
        table.unwrap()
    }

    fn chosen(route: &Route) -> Option<(Tier, &str)> {
        route
            .choice
            .as_ref()
            .map(|choice| (choice.tier, choice.model.model.as_str()))
    }

    #[test]
    fn partial_tables_keep_usable_entries_and_report_the_rest() {
        let (table, warnings) = RoutingTable::parse(&json!({
            "default_capability": "missing",
            "capabilities": {
                "code": {
                    "requires": ["vision", "telepathy", "min_context:100000"],
                    "tiers": {
                        "fast": {"provider": "openai", "model": "gpt-4o"},
                        "huge": {"provider": "openai", "model": "gpt-4o"},
                        "powerful": {"provider": "openai"}
                    }
                },
                "broken": "nope"
            }
        }));
        let table = table.unwrap();
        assert_eq!(table.capability_names(), ["code"]);
        assert_eq!(table.default_capability, None);
        let code = table.capability("code").unwrap();
        assert_eq!(
            code.requires,
            [Requirement::Vision, Requirement::MinContext(100_000)]
        );
        assert_eq!(code.tiers.len(), 1);
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        for expected in [
            "default_capability 'missing'",
            "telepathy",
            "'huge'",
            "'powerful'",
            "'broken'",
        ] {
            assert!(
                warnings.iter().any(|warning| warning.contains(expected)),
                "{expected} not in {warnings:?}"
            );
        }

        assert!(RoutingTable::parse(&Value::Null).0.is_none());
        assert!(RoutingTable::parse(&json!({"capabilities": {}}))
            .0
            .is_none());
        assert_eq!(RoutingTable::parse(&json!("text")).1.len(), 1);
    }

    #[test]
    fn parses_the_table_the_desktop_editor_saves() {
        let saved = r#"{"default_capability":"code","capabilities":{"research":{"notes":"kept as is","description":"Everyday tasks","tiers":{}},"code":{"description":"Code work","requires":["vision","reasoning","min_context:128000"],"extensions":["comfyui"],"instructions":"Return file paths.","tiers":{"balanced":{"provider":"anthropic","model":"claude-sonnet"}}}}}"#;
        let table = table(serde_json::from_str(saved).unwrap());
        assert_eq!(table.default_capability.as_deref(), Some("code"));
        let research = table.capability("research").unwrap();
        assert_eq!(research.description.as_deref(), Some("Everyday tasks"));
        assert!(research.tiers.is_empty());
        let code = table.capability("code").unwrap();
        assert_eq!(code.description.as_deref(), Some("Code work"));
        assert_eq!(
            code.requires,
            [
                Requirement::Vision,
                Requirement::Reasoning,
                Requirement::MinContext(128_000)
            ]
        );
        assert_eq!(code.extensions, ["comfyui"]);
        assert_eq!(code.instructions.as_deref(), Some("Return file paths."));
        assert_eq!(
            code.tiers,
            [(
                Tier::Balanced,
                RoutedModel {
                    provider: "anthropic".to_string(),
                    model: "claude-sonnet".to_string(),
                }
            )]
        );
        let research = table.route(Some("research"), None).unwrap().unwrap();
        assert_eq!(chosen(&research), Some((Tier::Balanced, "claude-sonnet")));
    }

    #[test]
    fn missing_tiers_fall_back_to_the_nearest_then_the_default_capability() {
        let table = table(json!({
            "default_capability": "general",
            "capabilities": {
                "general": {"tiers": {
                    "fast": {"provider": "openai", "model": "general-fast"},
                    "powerful": {"provider": "openai", "model": "general-powerful"}
                }},
                "code": {"tiers": {
                    "fast": {"provider": "openai", "model": "code-fast"},
                    "balanced": {"provider": "openai", "model": "code-balanced"}
                }},
                "review": {}
            }
        }));
        let route = |capability, tier| table.route(capability, tier).unwrap().unwrap();

        let exact = route(Some("code"), Some(Tier::Fast));
        assert_eq!(chosen(&exact), Some((Tier::Fast, "code-fast")));
        assert!(exact.notes.is_empty());
        assert_eq!(
            chosen(&route(Some("code"), None)),
            Some((Tier::Balanced, "code-balanced"))
        );
        let nearest = route(Some("code"), Some(Tier::Powerful));
        assert_eq!(chosen(&nearest), Some((Tier::Balanced, "code-balanced")));
        assert!(nearest.notes[0].contains("used code/balanced"));
        assert_eq!(
            chosen(&route(None, Some(Tier::Balanced))),
            Some((Tier::Powerful, "general-powerful"))
        );
        assert_eq!(
            chosen(&route(Some("review"), Some(Tier::Fast))),
            Some((Tier::Fast, "general-fast"))
        );
        let none = route(Some("review"), Some(Tier::Balanced));
        assert_eq!(none.choice, None);
        assert!(none.notes[0].contains("default subagent model"));

        assert!(table
            .route(Some("art"), None)
            .unwrap_err()
            .contains("Unknown capability 'art'"));
    }

    #[test]
    fn requirements_skip_models_that_lack_them_and_allow_unknown_metadata() {
        assert_eq!(
            unmet_requirements(
                "openai",
                "gpt-4",
                &[
                    Requirement::Vision,
                    Requirement::Reasoning,
                    Requirement::MinContext(100_000)
                ]
            ),
            [
                Requirement::Vision,
                Requirement::Reasoning,
                Requirement::MinContext(100_000)
            ]
        );
        assert_eq!(
            unmet_requirements("openai", "gpt-3.5-turbo", &[]),
            [Requirement::ToolCall]
        );
        assert!(unmet_requirements("openai", "gpt-4o", &[Requirement::Vision]).is_empty());
        assert!(unmet_requirements(
            "openai",
            "no-such-model-anywhere",
            &[Requirement::Vision, Requirement::Reasoning]
        )
        .is_empty());

        let table = table(json!({
            "capabilities": {
                "vision": {
                    "requires": ["vision"],
                    "tiers": {
                        "balanced": {"provider": "openai", "model": "gpt-4"},
                        "powerful": {"provider": "openai", "model": "gpt-4o"}
                    }
                },
                "strict": {
                    "requires": ["vision"],
                    "tiers": {"balanced": {"provider": "openai", "model": "gpt-4"}}
                },
                "unknown": {
                    "requires": ["vision", "reasoning"],
                    "tiers": {"balanced": {"provider": "openai", "model": "no-such-model-anywhere"}}
                }
            }
        }));
        let route = table.route(Some("vision"), None).unwrap().unwrap();
        assert_eq!(chosen(&route), Some((Tier::Powerful, "gpt-4o")));
        assert!(route.notes[0].contains("skipped openai/gpt-4"));
        assert!(route.notes[0].contains("vision"));
        let strict = table.route(Some("strict"), None).unwrap().unwrap();
        assert_eq!(strict.choice, None);
        assert_eq!(
            chosen(&table.route(Some("unknown"), None).unwrap().unwrap()),
            Some((Tier::Balanced, "no-such-model-anywhere"))
        );
        assert!(table.route(None, None).unwrap().is_none());
    }

    #[test]
    fn description_lists_capabilities_tiers_and_precedence() {
        let table = table(json!({
            "default_capability": "general",
            "capabilities": {
                "general": {"description": "Everyday tasks"},
                "image-generation": {"description": "Creating or editing images"}
            }
        }));
        let description = table.describe();
        for expected in [
            "- general: Everyday tasks (default)",
            "- image-generation: Creating or editing images",
            "- fast: lookups, one-line fixes",
            "- balanced: single-file edits, known bugs (used when `tier` is omitted)",
            "- powerful: multi-file refactors, architecture, security, long reasoning",
            "takes precedence over the table",
        ] {
            assert!(
                description.contains(expected),
                "{expected} not in {description}"
            );
        }
    }
}

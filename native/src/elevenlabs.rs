use crate::cloud_speech::is_elevenlabs_model_id;
use serde::Serialize;
use std::time::Duration;

const REALTIME_SPEC_URL: &str =
    "https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime.md";

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElevenLabsModelInfo {
    id: String,
    display_name: String,
}

pub async fn fetch_elevenlabs_models() -> Result<Vec<ElevenLabsModelInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|err| format!("Could not initialize ElevenLabs model discovery: {err}"))?;
    let docs = client
        .get(REALTIME_SPEC_URL)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| format!("Could not fetch the ElevenLabs realtime model list: {err}"))?
        .text()
        .await
        .map_err(|err| format!("Could not read the ElevenLabs realtime model list: {err}"))?;
    parse_realtime_models(&docs)
}

fn parse_realtime_models(docs: &str) -> Result<Vec<ElevenLabsModelInfo>, String> {
    // The Markdown reference contains the AsyncAPI schema for the realtime endpoint.
    // Read only its model ID enum. The general model catalog has no realtime STT flag.
    let schema: Vec<_> = docs
        .lines()
        .skip_while(|line| line.trim() != "```yaml")
        .skip(1)
        .take_while(|line| line.trim() != "```")
        .skip_while(|line| *line != "    _v1_speech-to-text_realtime_model_id:")
        .skip(1)
        .take_while(|line| line.starts_with("      ") || line.trim().is_empty())
        .collect();
    let mut ids: Vec<_> = schema
        .iter()
        .copied()
        .skip_while(|line| *line != "      enum:")
        .skip(1)
        .map_while(|line| line.strip_prefix("        - "))
        .map(|id| id.trim().trim_matches(['\'', '"']))
        .filter(|id| is_elevenlabs_model_id(id))
        .collect();
    if ids.is_empty() {
        return Err("The ElevenLabs realtime reference did not contain a model list.".into());
    }
    ids.sort_unstable();
    ids.dedup();
    let default = schema
        .iter()
        .find_map(|line| line.strip_prefix("      default: "))
        .map(|id| id.trim().trim_matches(['\'', '"']));
    if let Some(index) = ids.iter().position(|id| Some(*id) == default) {
        ids[..=index].rotate_right(1);
    }
    Ok(ids
        .into_iter()
        .map(|id| ElevenLabsModelInfo {
            id: id.to_string(),
            display_name: id
                .split(['_', '-'])
                .filter(|part| !part.is_empty())
                .map(|part| {
                    if part.strip_prefix('v').is_some_and(|version| {
                        !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
                    }) {
                        part.to_string()
                    } else {
                        format!("{}{}", part[..1].to_ascii_uppercase(), &part[1..])
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_realtime_model_ids_and_prioritizes_the_provider_default() {
        let docs = r#"
## AsyncAPI Specification
```yaml
components:
  schemas:
    _v1_speech-to-text_realtime_model_id:
      type: string
      enum:
        - scribe_v2_realtime
        - scribe_v2_realtime
        - 'scribe_v3_live'
        - fake&audio_format=ulaw
      default: scribe_v3_live
    _v1_speech-to-text_realtime_commit_strategy:
      enum:
        - vad
```
"#;
        let models = parse_realtime_models(docs).unwrap();
        assert_eq!(models[0].id, "scribe_v3_live");
        assert_eq!(models[0].display_name, "Scribe v3 Live");
        assert_eq!(models[1].id, "scribe_v2_realtime");
        assert_eq!(models.len(), 2);
        assert!(parse_realtime_models("Model catalog temporarily unavailable").is_err());
    }
}

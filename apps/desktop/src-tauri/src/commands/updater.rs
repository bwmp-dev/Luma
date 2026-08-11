use serde::{Deserialize, Serialize};
use tauri::{ipc::Channel, AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

use crate::errors::{LumaError, Result};

const STABLE_ENDPOINT: &str =
    "https://github.com/bwmp-dev/Luma/releases/latest/download/latest.json";
const NIGHTLY_ENDPOINT: &str =
    "https://github.com/bwmp-dev/Luma/releases/download/nightly/latest.json";

#[derive(Default)]
pub struct UpdaterState {
    pending: Mutex<Option<Update>>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Nightly,
}

impl UpdateChannel {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Stable => STABLE_ENDPOINT,
            Self::Nightly => NIGHTLY_ENDPOINT,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum UpdateDownloadEvent {
    Started {
        #[serde(rename = "contentLength")]
        content_length: Option<u64>,
    },
    Progress {
        #[serde(rename = "chunkLength")]
        chunk_length: usize,
    },
    Finished,
}

fn unavailable(error: impl std::fmt::Display) -> LumaError {
    tracing::warn!(%error, "updater operation failed");
    LumaError::UpdateUnavailable
}

#[tauri::command]
pub async fn updater_check(
    app: AppHandle,
    state: State<'_, UpdaterState>,
    channel: UpdateChannel,
) -> Result<Option<UpdateInfo>> {
    let endpoint = channel.endpoint().parse().map_err(unavailable)?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(unavailable)?
        .build()
        .map_err(unavailable)?;
    let update = updater.check().await.map_err(unavailable)?;

    let info = update.as_ref().map(|update| UpdateInfo {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
    });
    *state.pending.lock().await = update;
    Ok(info)
}

#[tauri::command]
pub async fn updater_download_and_install(
    state: State<'_, UpdaterState>,
    on_event: Channel<UpdateDownloadEvent>,
) -> Result<()> {
    let update = state
        .pending
        .lock()
        .await
        .take()
        .ok_or_else(|| LumaError::InvalidInput("no update is ready to install".into()))?;

    let mut started = false;
    let result = update
        .download_and_install(
            |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = on_event.send(UpdateDownloadEvent::Started { content_length });
                }
                let _ = on_event.send(UpdateDownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(UpdateDownloadEvent::Finished);
            },
        )
        .await;

    if let Err(error) = result {
        *state.pending.lock().await = Some(update);
        return Err(unavailable(error));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_use_independent_manifests() {
        assert!(UpdateChannel::Stable
            .endpoint()
            .ends_with("/latest/download/latest.json"));
        assert!(UpdateChannel::Nightly
            .endpoint()
            .ends_with("/nightly/latest.json"));
        assert_ne!(
            UpdateChannel::Stable.endpoint(),
            UpdateChannel::Nightly.endpoint()
        );
    }
}

use tauri::State;

use crate::errors::Result;
use crate::import::{self, ImportHostsRequest, ImportedHostCandidate, ImportedHostsResult};
use crate::keystore::KeystoreState;
use crate::AppState;

#[tauri::command]
pub async fn import_hosts_preview(
    state: State<'_, AppState>,
    source: String,
    // Absent for the `putty-live` source, which reads this machine's saved
    // PuTTY sessions instead of a file the user picked.
    path: Option<String>,
    vault_id: Option<String>,
) -> Result<Vec<ImportedHostCandidate>> {
    import::preview_hosts(
        &state.pool,
        source,
        path,
        vault_id
            .as_deref()
            .unwrap_or(crate::storage::vaults::PERSONAL_VAULT_ID),
    )
    .await
}

#[tauri::command]
pub async fn import_hosts_apply(
    state: State<'_, AppState>,
    keystore_state: State<'_, KeystoreState>,
    source: String,
    path: Option<String>,
    request: ImportHostsRequest,
) -> Result<ImportedHostsResult> {
    import::apply_hosts(&state.pool, &keystore_state, source, path, request).await
}

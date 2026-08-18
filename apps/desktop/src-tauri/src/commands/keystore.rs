use crate::errors::Result;
use crate::{
    keystore::{self, KeystoreState, KeystoreStatus},
    AppState,
};
use serde::Deserialize;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri::Manager;
use tauri::{AppHandle, State};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreSetupInput {
    password: String,
    remember_device: bool,
}
#[tauri::command]
pub async fn keystore_status(
    state: State<'_, AppState>,
    keystore_state: State<'_, KeystoreState>,
) -> Result<KeystoreStatus> {
    keystore::status(&state.pool, &keystore_state).await
}
#[tauri::command]
pub async fn keystore_setup(
    state: State<'_, AppState>,
    keystore_state: State<'_, KeystoreState>,
    input: KeystoreSetupInput,
) -> Result<()> {
    keystore::setup(
        &state.pool,
        &keystore_state,
        &input.password,
        input.remember_device,
    )
    .await
}
#[tauri::command]
pub async fn keystore_unlock(
    state: State<'_, AppState>,
    keystore_state: State<'_, KeystoreState>,
    password: String,
) -> Result<()> {
    keystore::unlock(&state.pool, &keystore_state, &password).await
}
#[tauri::command]
pub fn keystore_lock(app: AppHandle, keystore_state: State<'_, KeystoreState>) {
    keystore::lock(&keystore_state);
    // Locking the vault is how someone ends access to their credentials, so a
    // pane shared with an agent must not stay readable past it. Shares are
    // re-granted deliberately after unlocking rather than resuming silently.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(mcp) = app.try_state::<crate::mcp::McpState>() {
        mcp.revoke_all_shares();
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let _ = app;
}
#[tauri::command]
pub async fn keystore_set_policy(
    state: State<'_, AppState>,
    keystore_state: State<'_, KeystoreState>,
    remember_device: bool,
) -> Result<()> {
    keystore::set_policy(&state.pool, &keystore_state, remember_device).await
}

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::errors::{LumaError, Result};
use crate::mcp::taps::SharedPane;
use crate::mcp::{ActivityEntry, McpState, McpStatus};
use crate::ssh::EmbeddedSshManager;
use crate::storage::mcp_grants::{self, McpGrant};
use crate::AppState;

/// A newly created grant, with its token.
///
/// The token appears here and nowhere else, ever: only its digest is stored, so
/// there is no way to show it again. The UI has to make that clear.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGrantCreated {
    pub grant: McpGrant,
    pub token: String,
}

#[tauri::command]
pub async fn mcp_status(app: AppHandle, mcp: State<'_, McpState>) -> Result<McpStatus> {
    mcp.status(&app).await
}

#[tauri::command]
pub async fn mcp_grants_list(state: State<'_, AppState>) -> Result<Vec<McpGrant>> {
    mcp_grants::list(&state.pool).await
}

#[tauri::command]
pub async fn mcp_grant_create(
    app: AppHandle,
    state: State<'_, AppState>,
    mcp: State<'_, McpState>,
    label: String,
    host_ids: Vec<String>,
    require_approval: bool,
) -> Result<McpGrantCreated> {
    let generated = crate::mcp::generate_token();
    let grant = mcp_grants::create(
        &state.pool,
        &label,
        &generated.sha256,
        &generated.prefix,
        &host_ids,
        require_approval,
    )
    .await?;
    // Creating the first grant is what opens the endpoint.
    mcp.sync_lifecycle(&app).await?;
    Ok(McpGrantCreated {
        grant,
        token: generated.token,
    })
}

#[tauri::command]
pub async fn mcp_grant_update(
    state: State<'_, AppState>,
    mcp: State<'_, McpState>,
    id: String,
    label: String,
    host_ids: Vec<String>,
    require_approval: bool,
) -> Result<McpGrant> {
    let grant = mcp_grants::update(&state.pool, &id, &label, &host_ids, require_approval).await?;
    // Narrowing a grant must not leave an agent holding a pane it can no longer
    // be granted, so shares are dropped and the user re-shares deliberately.
    mcp.taps().unshare_all(Some(&id));
    Ok(grant)
}

#[tauri::command]
pub async fn mcp_grant_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    mcp: State<'_, McpState>,
    id: String,
) -> Result<()> {
    mcp_grants::delete(&state.pool, &id).await?;
    mcp.taps().unshare_all(Some(&id));
    mcp.approvals().deny_all();
    // Deleting the last grant closes the endpoint.
    mcp.sync_lifecycle(&app).await?;
    Ok(())
}

#[tauri::command]
pub async fn mcp_shared_panes(mcp: State<'_, McpState>) -> Result<Vec<SharedPane>> {
    Ok(mcp.taps().shared_panes(None))
}

#[tauri::command]
pub async fn mcp_pane_share(
    state: State<'_, AppState>,
    mcp: State<'_, McpState>,
    embedded: State<'_, EmbeddedSshManager>,
    session_id: String,
    grant_id: String,
    title: String,
) -> Result<()> {
    crate::ssh::validate_host_id(&session_id)?;
    // Confirms the grant exists rather than trusting an id from the webview.
    mcp_grants::get(&state.pool, &grant_id).await?;

    // A session is registered before authentication starts; sharing one that is
    // still connecting would expose the password prompt and let an agent type
    // into it.
    if matches!(
        mcp.taps().pane_kind(&session_id),
        Some(crate::mcp::PaneKind::Ssh { .. })
    ) && !embedded.is_authenticated(&session_id)
    {
        return Err(LumaError::InvalidInput(
            "this session is still connecting".into(),
        ));
    }

    if !mcp.taps().share(&session_id, &grant_id, &title) {
        return Err(LumaError::InvalidInput("unknown terminal session".into()));
    }
    Ok(())
}

#[tauri::command]
pub async fn mcp_pane_unshare(mcp: State<'_, McpState>, session_id: String) -> Result<()> {
    mcp.taps().unshare(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn mcp_approval_resolve(
    mcp: State<'_, McpState>,
    request_id: String,
    allowed: bool,
) -> Result<()> {
    mcp.approvals().resolve(&request_id, allowed);
    Ok(())
}

/// Report the session the webview opened for a pending `mcp-session-request`,
/// or the reason it could not.
///
/// The session id is validated and confirmed to be a live SSH session before it
/// is accepted: it arrives from the webview, and a tool call is about to type a
/// command into whatever it names.
#[tauri::command]
pub async fn mcp_session_ready(
    mcp: State<'_, McpState>,
    embedded: State<'_, EmbeddedSshManager>,
    request_id: String,
    session_id: Option<String>,
    error: Option<String>,
) -> Result<()> {
    let outcome = match session_id {
        Some(session_id) => {
            crate::ssh::validate_host_id(&session_id)?;
            if !embedded.is_authenticated(&session_id) {
                return Err(LumaError::InvalidInput(
                    "that session is not connected".into(),
                ));
            }
            Ok(session_id)
        }
        None => Err(error.unwrap_or_else(|| "Luma could not open a session".into())),
    };
    mcp.sessions().resolve(&request_id, outcome);
    Ok(())
}

#[tauri::command]
pub async fn mcp_activity_list(mcp: State<'_, McpState>) -> Result<Vec<ActivityEntry>> {
    Ok(mcp.activity())
}

/// Path an MCP client should launch to reach this Luma installation.
#[tauri::command]
pub async fn mcp_executable_path() -> Result<String> {
    crate::mcp::client_executable_path()
}

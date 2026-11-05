//! Ctrl+Alt+Del on behalf of the agent.
//!
//! Windows never treats injected keys as the secure attention sequence, and
//! only a service may call `SendSAS`. The agent asks the service over a
//! named pipe that only SYSTEM can open, with the requests in
//! `proto/inputweave/service/v1`. See the `policy` module for the
//! `SoftwareSASGeneration` policy this needs.

use std::io;
use std::time::Duration;

use iw_ipc::{IpcConnection, IpcListener, PipeAccess};
use iw_proto::service::v1::{SendSas, ServiceRequest, ServiceResponse, service_request::Request};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Security::Authentication::Identity::SendSAS;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::HSTRING;

use crate::policy;

/// The service's pipe.
pub const PIPE: &str = r"\\.\pipe\InputWeave\service";

/// How long the agent waits for the service, per step.
const TIMEOUT: Duration = Duration::from_secs(2);

const ACCEPT_RETRY: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum SasError {
    #[error("cannot reach the InputWeave service: {0}")]
    Unreachable(io::Error),
    #[error("the InputWeave service did not answer: {0}")]
    NoAnswer(String),
    #[error("the InputWeave service could not send Ctrl+Alt+Del: {0}")]
    Refused(String),
}

/// Asks the service to send Ctrl+Alt+Del. Blocks for up to a few seconds,
/// on a runtime of its own, so call it from a thread that is not running
/// async code.
pub fn request() -> Result<(), SasError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(SasError::Unreachable)?;
    runtime.block_on(async {
        let timed_out = || SasError::NoAnswer("timed out".to_owned());
        let mut conn = tokio::time::timeout(TIMEOUT, IpcConnection::connect(PIPE))
            .await
            .map_err(|_| timed_out())?
            .map_err(SasError::Unreachable)?;
        let request = ServiceRequest {
            request: Some(Request::SendSas(SendSas {})),
        };
        conn.send(&request)
            .await
            .map_err(|e| SasError::NoAnswer(e.to_string()))?;
        let response = tokio::time::timeout(TIMEOUT, conn.recv::<ServiceResponse>())
            .await
            .map_err(|_| timed_out())?
            .map_err(|e| SasError::NoAnswer(e.to_string()))?
            .ok_or_else(|| SasError::NoAnswer("the connection closed".to_owned()))?;
        if response.error.is_empty() {
            Ok(())
        } else {
            Err(SasError::Refused(response.error))
        }
    })
}

/// Answers agents' requests until the process exits, on a runtime of its
/// own on the calling thread.
pub(crate) fn serve() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = IpcListener::bind_with(PIPE, PipeAccess::SystemOnly)?;
        accept(listener).await;
        Ok(())
    })
}

async fn accept(listener: IpcListener) {
    loop {
        match listener.accept().await {
            Ok(conn) => {
                tokio::spawn(answer(conn));
            }
            Err(e) => {
                tracing::warn!(error = %e, "service pipe accept failed");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

async fn answer(mut conn: IpcConnection) {
    while let Ok(Some(request)) = conn.recv::<ServiceRequest>().await {
        let error = match request.request {
            Some(Request::SendSas(_)) => send_sas().err().unwrap_or_default(),
            None => "unknown request".to_owned(),
        };
        if conn.send(&ServiceResponse { error }).await.is_err() {
            break;
        }
    }
}

fn send_sas() -> Result<(), String> {
    let setting = read_policy();
    if !policy::services_may_send_sas(setting) {
        return Err(format!(
            "the {} policy is {}; it must be 1 or 3 for services to send Ctrl+Alt+Del",
            policy::VALUE,
            setting.map_or("not set".to_owned(), |v| v.to_string()),
        ));
    }
    // SAFETY: no pointers. FALSE because the caller is a service, not an
    // application running as the user.
    unsafe { SendSAS(false) };
    tracing::info!("sent Ctrl+Alt+Del for the agent");
    Ok(())
}

fn read_policy() -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the buffer is a u32 and `size` says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(policy::KEY),
            &HSTRING::from(policy::VALUE),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}

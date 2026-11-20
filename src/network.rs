use crate::protocol::*;
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub fn random_secret() -> String {
    URL_SAFE_NO_PAD.encode(iroh::SecretKey::generate().to_bytes())
}
pub fn digest(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}
pub fn validate_key(key: &str) -> Result<()> {
    ensure!(
        key.len() == 43 && URL_SAFE_NO_PAD.decode(key).is_ok_and(|b| b.len() == 32),
        "invalid network credential"
    );
    Ok(())
}
pub fn same_key(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |v, (a, b)| v | (a ^ b)) == 0
}
pub fn validate_ticket(ticket: &Ticket) -> Result<()> {
    ensure!(ticket.version == 1, "unsupported ticket version");
    validate_text(&ticket.name, "machine name", 128)?;
    ensure!(
        serde_json::to_vec(ticket)?.len() <= 4096,
        "member ticket too large"
    );
    Ok(())
}
pub fn encode(invitation: &Invitation) -> Result<String> {
    Ok(format!(
        "am1_{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(invitation)?)
    ))
}
pub fn decode(code: &str) -> Result<Invitation> {
    let code = code.trim();
    ensure!(code.len() <= 8192, "invitation code too large");
    let encoded = code
        .strip_prefix("am1_")
        .ok_or_else(|| anyhow::anyhow!("expected an am1_ invitation code"))?;
    let invitation: Invitation = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| anyhow::anyhow!("invalid invitation encoding"))?,
    )
    .map_err(|_| anyhow::anyhow!("invalid invitation format"))?;
    ensure!(
        invitation.version == 1 && invitation.expires_at > now(),
        "invitation is expired or unsupported"
    );
    validate_key(&invitation.secret)?;
    validate_ticket(&invitation.inviter)?;
    Ok(invitation)
}

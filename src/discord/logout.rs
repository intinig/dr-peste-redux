use super::{Context, Error};

/// Disconnect your PoE account (removes your stored session).
#[poise::command(slash_command)]
pub async fn logout(ctx: Context<'_>) -> Result<(), Error> {
    let uid = ctx.author().id.get();
    ctx.data().sessions.forget(uid);
    ctx.send(poise::CreateReply::default().ephemeral(true).content(
        "Disconnected — your session is removed from memory. \
             For full safety, also log out on pathofexile.com to invalidate the cookie.",
    ))
    .await?;
    Ok(())
}

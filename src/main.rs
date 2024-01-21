use std::env;
use std::str::FromStr;
use std::time::Duration;

use ethers::types::H160;
use hyperliquid_rust_sdk::{BaseUrl, InfoClient, Message, Subscription};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::spawn;
use tokio::{sync::mpsc::unbounded_channel, time::sleep};
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct RelationshipData {
    child_addresses: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
struct Relationship {
    data: RelationshipData,
}

#[derive(Deserialize, Clone, Debug)]
struct Info {
    relationship: Relationship,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct InfoRequest {
    #[serde(rename = "type")]
    type_: String,
    vault_address: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_line_number(true)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    tracing_log::LogTracer::init()?;

    info!("Initializing client...");
    let mut info_client = InfoClient::new(None, Some(BaseUrl::Mainnet)).await?;

    let vault_address = "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303".to_string();
    let req = InfoRequest {
        type_: "vaultDetails".to_string(),
        vault_address,
    };
    let info_payload = info_client
        .http_client
        .post("/info", serde_json::to_string(&req)?)
        .await?;
    let info: Info = serde_json::from_str(&info_payload)?;
    let addresses = info.relationship.data.child_addresses;

    info!("Subscribing user events...");
    let (sender, mut receiver) = unbounded_channel();

    let mut subscribed_users: Vec<H160> = Vec::new();
    let mut subscription_ids: Vec<u32> = Vec::new();
    for address in addresses {
        let user = H160::from_str(address.as_str())?;
        let res = info_client
            .subscribe(Subscription::UserEvents { user }, sender.clone())
            .await;
        match res {
            Ok(u32) => {
                subscribed_users.push(user);
                subscription_ids.push(u32);
            }
            Err(e) => warn!("failed to subscribe: {e:?}"),
        }
    }

    spawn(async move {
        loop {
            sleep(Duration::from_secs(30)).await;

            info!("Resubscribing...");

            let mut failed_subscription_ids: Vec<u32> = Vec::new();
            let mut new_subscription_ids: Vec<u32> = Vec::new();
            for (i, subscription_id) in subscription_ids.iter().enumerate() {
                if info_client.unsubscribe(*subscription_id).await.is_err() {
                    warn!("failed to unsubscribe {subscription_id:?}");
                    failed_subscription_ids.push(*subscription_id);
                    continue;
                }
                let get_user_opt = subscribed_users.get(i);
                if get_user_opt.is_none() {
                    continue;
                }
                let user = get_user_opt.unwrap();
                let subscribe_res = info_client
                    .subscribe(Subscription::UserEvents { user: *user }, sender.clone())
                    .await;
                if subscribe_res.is_err() {
                    warn!("failed to subscribe {subscription_id:?}");
                    failed_subscription_ids.push(*subscription_id);
                    continue;
                }
                new_subscription_ids.push(subscribe_res.unwrap())
            }
            new_subscription_ids.append(&mut failed_subscription_ids);
            new_subscription_ids.dedup();
            subscription_ids = new_subscription_ids;
        }
    });

    let client = reqwest::Client::new();
    let discord_webhook_url = env::var("DISCORD_WEBHOOK_URL")?;

    loop {
        match receiver.recv().await {
            Some(Message::User(user)) => {
                for trade in user.data.fills {
                    let side = match trade.side.as_str() {
                        "A" => "Long",
                        "B" => "Short",
                        _ => "Unknown",
                    };
                    let message = format!("{} {} {}", side, trade.coin, trade.sz);
                    _ = client
                        .post(&discord_webhook_url)
                        .json(&json!({"content":message}))
                        .send()
                        .await?
                        .error_for_status()?;

                    sleep(Duration::from_secs(5)).await;
                }
            }
            _ => (),
        }
    }
}

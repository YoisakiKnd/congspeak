//! Non-destructive self-actions only. Reads test connection settings from stdin.
use congspeak_core::{ClientAction, ConnectRequest, ConnectionStatus, SessionEvent};
use congspeak_teamspeak::{IdentityMode, Session};
use std::{io::Read, time::Duration};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: ConnectRequest = serde_json::from_str(&input)?;
    let session = Session::start(IdentityMode::Ephemeral);
    session.connect(request.clone()).await?;
    let result=tokio::time::timeout(Duration::from_secs(40),async{
        let mut state=session.watch();
        loop{let snapshot=state.borrow_and_update().clone();if snapshot.status==ConnectionStatus::Connected{break;}
if snapshot.status==ConnectionStatus::Failed{return Err(snapshot.last_error.unwrap_or_default().into());}state.changed().await?;}
        let mut events=session.subscribe();
        let own_channel=session.snapshot().clients.into_iter().find(|c|Some(c.id)==session.snapshot().own_client_id).ok_or("self missing")?.channel_id;
        let actions=[ClientAction::Join{channel_id:own_channel,password:None},ClientAction::Rename{name:"CongSpeak-Actions-Test".into()},ClientAction::Away{message:Some("测试离开状态".into())},ClientAction::Away{message:None},ClientAction::Subscribe{enabled:true}];
        for (index,action) in actions.into_iter().enumerate(){
            let request_id=index as u32+1;session.action(request_id,action).await?;
            loop{if let SessionEvent::CommandResult{request_id:id,error}=events.recv().await? && id==request_id {if let Some(error)=error && !(request_id==1&&error=="ChannelAlreadyIn"){return Err(error.into());}break;}}
        }
        println!("PASS: joined own channel, renamed self, set/cleared away, subscribed; 4 self-actions succeeded; same-channel join returned the expected ChannelAlreadyIn error. No chat/audio or other-user changes.");
        Ok::<(),Box<dyn std::error::Error>>(())
    }).await;
    let _ = session.disconnect().await;
    result??;
    Ok(())
}

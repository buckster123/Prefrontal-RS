//! `cargo run -p prefrontal-client --example watch`
//! Lists projects once, then follows live events until Ctrl-C.

use futures_util::StreamExt;
use prefrontal_client::{Event, Prefrontal};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pf = Prefrontal::default();

    for p in pf.projects().await? {
        println!("{:<28} {:?}", p.name, p.activity);
    }

    println!("\nwatching for changes…");
    let mut events = std::pin::pin!(pf.events().await?);
    while let Some(event) = events.next().await {
        match event {
            Event::Snapshot { projects } => println!("snapshot: {} projects", projects.len()),
            Event::ProjectChanged { project } => println!("changed: {}", project.name),
            Event::ProjectRemoved { path } => println!("removed: {path}"),
        }
    }
    Ok(())
}

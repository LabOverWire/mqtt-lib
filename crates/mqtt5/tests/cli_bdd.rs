mod bdd;
mod common;

use bdd::BddWorld;
use cucumber::World;

#[tokio::main]
async fn main() {
    BddWorld::cucumber()
        .before(|_feature, _rule, _scenario, world| {
            Box::pin(async move {
                world.kill_sub_processes();
            })
        })
        .after(|_feature, _rule, _scenario, _event, world| {
            Box::pin(async move {
                if let Some(w) = world {
                    w.kill_sub_processes();
                }
            })
        })
        .run("tests/bdd/features/")
        .await;
}

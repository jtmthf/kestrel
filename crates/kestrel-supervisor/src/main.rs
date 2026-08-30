use std::collections::BTreeMap;

use kestrel_supervisor::{Stderr, run};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let variables: BTreeMap<String, String> = std::env::vars().collect();

    std::process::exit(run(&Stderr, &variables).await);
}

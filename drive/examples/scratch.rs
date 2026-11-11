//! Makes (or trashes) a scratch folder at the top of the real iCloud Drive,
//! for trying a sync against one folder: `scratch` prints the new folder's
//! id; `scratch trash <id>` moves it to Recently Deleted.
use icloud_drive::remote::{DriveApi, ROOT_ID, Remote};

fn main() {
    let api = DriveApi::connect().expect("signed in");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("trash") => {
            let (_, kids) = api.list(ROOT_ID).expect("listing");
            let node = kids
                .into_iter()
                .find(|k| k.id == args[1])
                .expect("no such folder at the top");
            api.trash(&node).expect("trash");
            println!("trashed {}", node.name);
        }
        _ => {
            let node = api
                .create_folder(ROOT_ID, &format!("icloud-drive-scratch-{}", std::process::id()))
                .expect("create");
            println!("{}", node.id);
        }
    }
}

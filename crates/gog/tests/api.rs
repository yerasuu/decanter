// SPDX-License-Identifier: GPL-3.0-only
// Based on gog 0.5.0 (gogapi-rs) by nicohman, GPL-3.0-only; see api/VENDOR.md
// Upstream style, kept as published (see src/api/VENDOR.md): the workspace's clippy groups are not applied to it.
#![allow(clippy::all, clippy::pedantic, clippy::restriction, clippy::nursery)]
use decanter_gog::api::gog::FilterParam::*;
use decanter_gog::api::gog::OS::*;
use decanter_gog::api::gog::*;
use decanter_gog::api::token::Token;
use decanter_gog::api::*;
use std::env::var_os;
use std::fs::File;
use std::io::Read;

// Some tests need an owned game.
// "Beneath a Steel Sky" is free from GOG, so a good test case.
const OWNED_GAME_ID: i64 = 1207658695;

fn get_gog() -> Gog {
    let path = var_os("GOG_TOKEN_PATH").unwrap().into_string().unwrap();
    let mut token_json = String::new();
    File::open(path)
        .unwrap()
        .read_to_string(&mut token_json)
        .unwrap();
    let mut token = Token::from_response(token_json.as_str()).unwrap();
    token = token.refresh().unwrap();
    Gog::new(token)
}

#[test]
#[ignore = "needs a GOG account and network"]
fn get_games() {
    let gog = get_gog();
    gog.get_games().unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn get_pub_info() {
    let gog = get_gog();
    let exp: Vec<String> = vec![
        "friendStatus".to_string(),
        "wishlistStatus".to_string(),
        "blockedStatus".to_string(),
    ];
    gog.get_pub_info(49171277422358, exp).unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn get_game_details() {
    let gog = get_gog();
    gog.get_game_details(OWNED_GAME_ID).unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn wishlist() {
    let gog = get_gog();
    gog.wishlist().unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn add_wishlist() {
    let gog = get_gog();
    gog.add_wishlist(1096313866).unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn rm_wishlist() {
    let gog = get_gog();
    gog.rm_wishlist(1096313866).unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn product() {
    let exp_game: Vec<String> = vec![
        "downloads".to_string(),
        "description".to_string(),
        "screenshots".to_string(),
        "related_products".to_string(),
    ];
    let gog = get_gog();
    gog.product(vec![1452598881, 1096313866], exp_game).unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn game_ratings() {
    let gog = get_gog();
    gog.game_ratings().unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn filtered() {
    let gog = get_gog();
    gog.get_filtered_products(FilterParams::from_one(MediaType(1)))
        .unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn filtered_all() {
    let gog = get_gog();
    gog.get_all_filtered_products(FilterParams::from_one(MediaType(1)))
        .unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn filtered_os() {
    let gog = get_gog();
    gog.get_filtered_products(FilterParams::from_one(OS(Linux)))
        .unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn download() {
    let gog = get_gog();
    let details = gog.get_game_details(OWNED_GAME_ID).unwrap();
    gog.download_game(details.downloads.linux.unwrap());
}

#[test]
#[ignore = "needs a GOG account and network"]
fn filtered_search() {
    let gog = get_gog();
    gog.get_filtered_products(FilterParams::from_one(Search("Not A Hero".to_string())))
        .unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn filtered_unowned() {
    let gog = get_gog();
    gog.get_products(FilterParams::from_one(Search("Stellaris".to_string())))
        .unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn connect_status() {
    let gog = get_gog();
    let uid = gog.get_user_data().unwrap().user_id;
    let status = gog.connect_status(uid.parse().unwrap());
    if status.is_err() {
        let err = status.err().unwrap();
        match err.kind() {
            ErrorKind::NotAvailable => {}
            _ => panic!("{:?}", err),
        }
    }
}

#[test]
#[ignore = "needs a GOG account and network"]
fn friends() {
    let gog = get_gog();
    gog.friends().unwrap();
}

#[test]
#[ignore = "needs a GOG account and network"]
fn extract_data() {
    let gog = get_gog();
    let details = gog.get_game_details(OWNED_GAME_ID).unwrap();
    assert!(gog.extract_data(details.downloads.linux.unwrap()).is_ok())
}

#[test]
#[ignore]
fn extract() {
    use decanter_gog::api::extract::*;
    let mut in_file = File::open("beneath_a_steel_sky_en_gog_2_20150.sh").unwrap();
    extract(
        &mut in_file,
        ".",
        ToExtract {
            unpacker: true,
            data: true,
            mojosetup: true,
        },
    )
    .unwrap();
}

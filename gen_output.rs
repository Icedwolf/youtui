use std::io::Write;
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::ArtistChannelID;
use ytmapi_rs::parse::artist::GetArtist;
use ytmapi_rs::process_json;
use ytmapi_rs::query::GetArtistQuery;

fn main() {
    for (json_path, out_path) in [
        ("ytmapi-rs/test_json/get_artist_20250310.json", "ytmapi-rs/test_json/get_artist_20250310_output.txt"),
        ("ytmapi-rs/test_json/get_artist_20240705.json", "ytmapi-rs/test_json/get_artist_20240705_output.txt"),
    ] {
        let json = std::fs::read_to_string(json_path).unwrap();
        let result: GetArtist = process_json::<_, BrowserToken>(
            json,
            GetArtistQuery::new(ArtistChannelID::from_raw("")),
        ).unwrap();
        let debug = format!("{:#?}", result);
        std::fs::write(out_path, &debug).unwrap();
        println!("Wrote {}", out_path);
    }
}

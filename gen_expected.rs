fn main() {
    let paths = [
        ("ytmapi-rs/test_json/get_artist_20250310.json", "ytmapi-rs/test_json/get_artist_20250310_output.txt"),
        ("ytmapi-rs/test_json/get_artist_20240705.json", "ytmapi-rs/test_json/get_artist_20240705_output.txt"),
    ];
    for (json_path, out_path) in paths {
        let json = std::fs::read_to_string(json_path).unwrap();
        let result: ytmapi_rs::parse::artist::GetArtist = ytmapi_rs::process_json::<_, ytmapi_rs::auth::BrowserToken>(
            json,
            ytmapi_rs::query::GetArtistQuery::new(ytmapi_rs::common::ArtistChannelID::from_raw("")),
        ).unwrap();
        let debug = format!("{:#?}", result);
        std::fs::write(out_path, &debug).unwrap();
        eprintln!("Wrote {}", out_path);
    }
}

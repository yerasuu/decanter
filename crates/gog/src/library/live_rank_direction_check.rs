/// Not run by default. Opt in with `ACHV_LIVE_PROFILE_ID=<uuid>
/// cargo test -p achv-gog -- --ignored --nocapture rank_direction`
/// against a real, logged-in-to-GOG profile.
///
/// `sortBy=date_purchased` isn't documented well enough to know
/// whether index 0 means newest or oldest, and the response carries
/// no date to check directly -- so this cross-checks rank order
/// against `orders.rs`'s real (if narrower) purchase dates for
/// whatever subset of the library went through an actual order, and
/// reports how often the two agree.
#[test]
#[ignore]
fn rank_direction_matches_real_purchase_dates() {
    let profile_id = std::env::var("ACHV_LIVE_PROFILE_ID").expect("set ACHV_LIVE_PROFILE_ID");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (products, purchased) = rt.block_on(async {
        let (_owned_ids, access_token) = crate::with_session(&profile_id, |client| {
            let owned_ids = crate::achv_try!(client.get_games().map_err(|err| anyhow::anyhow!("{err}")));
            Ok((owned_ids, client.token.borrow().access_token.clone()))
        })
        .await
        .expect("with_session");

        let mut products = Vec::new();
        super::each_product_page(&access_token, async |page| products.extend(page))
            .await
            .expect("each_product_page");
        let purchased = crate::orders::purchase_dates(&profile_id)
            .await
            .expect("purchase_dates");
        (products, purchased)
    });

    let ranked: Vec<(i64, i64)> = products
        .iter()
        .enumerate()
        .filter_map(|(rank, product)| purchased.get(&product.id.to_string()).map(|&date| (rank as i64, date)))
        .collect();
    println!(
        "{} of {} products have a real purchase date to check against",
        ranked.len(),
        products.len()
    );
    assert!(
        ranked.len() >= 2,
        "not enough overlap between the ranked listing and real order history to say anything"
    );

    let mut agree = 0i64;
    let mut disagree = 0i64;
    for i in 0..ranked.len() {
        for j in (i + 1)..ranked.len() {
            let (rank_i, date_i) = ranked[i];
            let (rank_j, date_j) = ranked[j];
            // If rank 0 is genuinely "most recent", a smaller rank
            // should pair with a larger (later) purchase date.
            let ranked_newer = rank_i.cmp(&rank_j).reverse();
            let dated_newer = date_i.cmp(&date_j);
            if ranked_newer == dated_newer {
                agree += 1;
            } else if ranked_newer == dated_newer.reverse() {
                disagree += 1;
            }
        }
    }
    let total = agree + disagree;
    println!(
        "rank-ascending vs date_purchased-descending: {agree}/{total} pairs agree ({:.0}%)",
        100.0 * agree as f64 / total as f64
    );
    // Report only -- deliberately no assert on the ratio itself. The
    // number decides which way `fetch_products_by_added_order`'s
    // result should be read; that's a judgment call to make from the
    // printed output, not something to hardcode a threshold for here.
}

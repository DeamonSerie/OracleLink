use crate::{mock::*, AssetKind, Error, OracleReadError};
use frame_support::{assert_noop, assert_ok};

fn setup_oracle_and_contract(oracle_id: u64, contract: u64, domain: &[u8], price: u128) {
    set_contract(contract, true);
    set_oracle(oracle_id, true);
    set_price(oracle_id, domain, Ok(price));
}

fn register(
    contract: u64,
    oracle_id: u64,
    domain: &[u8],
    price: u128,
    kind: AssetKind,
    supply: u128,
) {
    setup_oracle_and_contract(oracle_id, contract, domain, price);
    assert_ok!(AssetWeight::register_asset(
        RuntimeOrigin::signed(1),
        contract,
        kind,
        oracle_id,
        domain.to_vec(),
        0,
        supply
    ));
}

fn signed_delta(before: u128, after: u128) -> i128 {
    if after >= before {
        (after - before) as i128
    } else {
        -((before - after) as i128)
    }
}

#[test]
fn register_requires_active_oracle() {
    new_test_ext().execute_with(|| {
        set_contract(1, true);
        set_oracle(7, false);
        set_price(7, b"price", Ok(100));
        assert_noop!(
            AssetWeight::register_asset(
                RuntimeOrigin::signed(1),
                1,
                AssetKind::Fungible,
                7,
                b"price".to_vec(),
                6,
                1_000_000
            ),
            Error::<Test>::OracleInactive
        );
    });
}

#[test]
fn register_requires_contract() {
    new_test_ext().execute_with(|| {
        set_contract(1, false);
        set_oracle(7, true);
        set_price(7, b"price", Ok(100));
        assert_noop!(
            AssetWeight::register_asset(
                RuntimeOrigin::signed(1),
                1,
                AssetKind::Fungible,
                7,
                b"price".to_vec(),
                6,
                1_000_000
            ),
            Error::<Test>::ContractNotFound
        );
    });
}

#[test]
fn register_requires_readable_price() {
    new_test_ext().execute_with(|| {
        set_contract(1, true);
        set_oracle(7, true);
        set_price(7, b"price", Err(OracleReadError::NoPrice));
        assert_noop!(
            AssetWeight::register_asset(
                RuntimeOrigin::signed(1),
                1,
                AssetKind::Fungible,
                7,
                b"price".to_vec(),
                6,
                1_000_000
            ),
            Error::<Test>::NoLedgerPrice
        );
    });
}

#[test]
fn register_sets_effective_price_from_oracle() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 1234, AssetKind::Fungible, 1_000);
        let s = AssetWeight::asset_state(10);
        assert_eq!(s.last_price, 1234);
        assert_eq!(s.effective_price, 1234);
    });
}

#[test]
fn refresh_rejects_conflicted_or_invalid_oracle_as_before() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 100, AssetKind::Fungible, 1_000);
        set_price(1, b"p", Err(OracleReadError::Conflicted));
        assert_noop!(
            AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10),
            Error::<Test>::PriceConflicted
        );
        set_price(1, b"p", Err(OracleReadError::InvalidPayload));
        assert_noop!(
            AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10),
            Error::<Test>::InvalidPricePayload
        );
    });
}

#[test]
fn self_influence_still_applies_and_is_separate() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 10_000, AssetKind::Fungible, 1_000);
        let before = AssetWeight::asset_state(10).effective_price;
        assert_ok!(AssetWeight::record_fungible_transfer(
            RuntimeOrigin::signed(1),
            10,
            None
        ));
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after = AssetWeight::asset_state(10);
        assert!(
            after.effective_price > before,
            "self influence should increase source price"
        );
        assert_eq!(
            after.last_price, 10_000,
            "oracle last_price remains oracle-sourced"
        );
    });
}

#[test]
fn dissimilar_assets_receive_larger_share_than_similar() {
    new_test_ext().execute_with(|| {
        // Source base 50. Near target base 60 (distance 10), far target base 140 (distance 90).
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_060);
        register(12, 3, b"c", 1_000, AssetKind::Fungible, 1_140);
        let near_before = AssetWeight::asset_state(11).effective_price;
        let far_before = AssetWeight::asset_state(12).effective_price;

        set_price(1, b"a", Ok(994)); // delta_w = +6, total budget = 3
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let near_delta = AssetWeight::asset_state(11).effective_price - near_before;
        let far_delta = AssetWeight::asset_state(12).effective_price - far_before;
        assert!(
            far_delta > near_delta,
            "far should get larger share before caps equalize: near={near_delta} far={far_delta}"
        );
        assert!(near_delta > 0);
    });
}

#[test]
fn same_band_big_bunch_hits_cap_and_overflows_to_other_bands() {
    new_test_ext().execute_with(|| {
        // Source base 50. Two dissimilar targets in same band (100s band): 140 and 141.
        // A third target in another band: 200.
        // Bunch cap 2, total budget 5 => first band capped, overflow to 200-band.
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        register(12, 3, b"c", 1_000, AssetKind::Fungible, 1_141);
        register(13, 4, b"d", 1_000, AssetKind::Fungible, 1_200);
        let b_before = AssetWeight::asset_state(11).effective_price;
        let c_before = AssetWeight::asset_state(12).effective_price;
        let d_before = AssetWeight::asset_state(13).effective_price;

        set_price(1, b"a", Ok(990)); // delta_w +10 => budget 5
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let b_delta = AssetWeight::asset_state(11).effective_price - b_before;
        let c_delta = AssetWeight::asset_state(12).effective_price - c_before;
        let d_delta = AssetWeight::asset_state(13).effective_price - d_before;
        let capped_band = b_delta + c_delta;
        assert!(
            capped_band <= 2,
            "same-band bunch should be capped: {capped_band}"
        );
        assert!(d_delta > 0, "overflow should reach other band: {d_delta}");
    });
}

#[test]
fn overflow_redistributes_same_sign_to_remaining_bunches() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        register(12, 3, b"c", 1_000, AssetKind::Fungible, 1_141);
        register(13, 4, b"d", 1_000, AssetKind::Fungible, 1_200);
        let b0 = AssetWeight::asset_state(11).effective_price;
        let c0 = AssetWeight::asset_state(12).effective_price;
        let d0 = AssetWeight::asset_state(13).effective_price;

        set_price(1, b"a", Ok(980)); // delta_w +20 => budget 10, still capped by total budget
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let b = signed_delta(b0, AssetWeight::asset_state(11).effective_price);
        let c = signed_delta(c0, AssetWeight::asset_state(12).effective_price);
        let d = signed_delta(d0, AssetWeight::asset_state(13).effective_price);
        assert!(
            b >= 0 && c >= 0 && d >= 0,
            "all deltas same-sign: b={b} c={c} d={d}"
        );
        assert!(
            b + c <= 2,
            "capped bunch should not exceed cap: {} (b={b} c={c} d={d})",
            b + c
        );
        assert!(d > 0, "remaining bunch should receive overflow: {d}");
    });
}

#[test]
fn identical_or_near_identical_receive_minimal_residual_share() {
    new_test_ext().execute_with(|| {
        // Source 50, identical target 50 (d==0), dissimilar targets consume/cap budget.
        // Two dissimilar targets in same band with cap 2, total budget 5.
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"same", 1_000, AssetKind::Fungible, 1_050); // identical
        register(12, 3, b"far1", 1_000, AssetKind::Fungible, 1_140);
        register(13, 4, b"far2", 1_000, AssetKind::Fungible, 1_141);
        register(14, 5, b"far3", 1_000, AssetKind::Fungible, 1_200);
        let same_before = AssetWeight::asset_state(11).effective_price;
        let far3_before = AssetWeight::asset_state(14).effective_price;

        set_price(1, b"a", Ok(990)); // budget 5
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let same_delta = AssetWeight::asset_state(11).effective_price - same_before;
        let far3_delta = AssetWeight::asset_state(14).effective_price - far3_before;
        // Identical target should only get epsilon residual after d>0 capacity is considered.
        // Far3 receives overflow in d>0 pass; identical should not exceed any d>0 recipient.
        assert!(
            same_delta <= far3_delta,
            "identical must not exceed more-dissimilar share: same={same_delta} far3={far3_delta}"
        );
    });
}

#[test]
fn fungible_and_nft_participate_in_band_bunching() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050); // base 50
                                                                  // NFT with base 50: price-supply-transfer = 1100-1050 = 50
        register(11, 2, b"b", 1_100, AssetKind::NonFungible, 1_050);
        // Dissimilar fungible
        register(12, 3, b"c", 1_000, AssetKind::Fungible, 1_140);
        let nft_before = AssetWeight::asset_state(11).effective_price;
        let far_before = AssetWeight::asset_state(12).effective_price;

        set_price(1, b"a", Ok(990)); // +10 delta
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let nft_delta = signed_delta(nft_before, AssetWeight::asset_state(11).effective_price);
        let far_delta = signed_delta(far_before, AssetWeight::asset_state(12).effective_price);
        assert!(far_delta > 0, "dissimilar target should receive impact");
        // NFT is identical base (d==0), so only epsilon residual at most.
        assert!(
            nft_delta <= far_delta,
            "identical NFT should not exceed dissimilar target: nft={nft_delta} far={far_delta}"
        );
    });
}

#[test]
fn per_asset_and_price_bounds_respected_during_overflow() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        // Near ceiling target in dissimilar band; headroom clamps it.
        register(12, 3, b"c", 999_999, AssetKind::Fungible, 1_200);
        let c_before = AssetWeight::asset_state(12).effective_price;
        set_price(1, b"a", Ok(980)); // larger delta
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let c_after = AssetWeight::asset_state(12);
        assert!(c_after.effective_price <= 1_000_000);
        assert!(c_after.effective_price >= c_before);
    });
}

#[test]
fn total_absorbed_never_exceeds_total_budget() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        register(12, 3, b"c", 1_000, AssetKind::Fungible, 1_141);
        register(13, 4, b"d", 1_000, AssetKind::Fungible, 1_200);
        let before: Vec<u128> = [11u64, 12, 13]
            .iter()
            .map(|c| AssetWeight::asset_state(*c).effective_price)
            .collect();
        set_price(1, b"a", Ok(980)); // delta_w 20 => budget 10
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after: Vec<u128> = [11u64, 12, 13]
            .iter()
            .map(|c| AssetWeight::asset_state(*c).effective_price)
            .collect();
        let absorbed: u128 = before.iter().zip(after.iter()).map(|(b, a)| a - b).sum();
        assert!(
            absorbed <= 10,
            "absorbed {absorbed} exceeded total budget 10"
        );
    });
}

#[test]
fn negative_delta_is_same_sign_and_clamped() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"a", 1_000, AssetKind::Fungible, 1_050);
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        let before = AssetWeight::asset_state(11).effective_price;
        set_price(1, b"a", Ok(1_020)); // price up => fungible base weight down
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after = AssetWeight::asset_state(11).effective_price;
        assert!(
            after < before,
            "negative source delta should reduce dissimilar target: {before}->{after}"
        );
    });
}

#[test]
fn distribution_aggregate_is_deterministic_and_o1_shape() {
    new_test_ext().execute_with(|| {
        let a = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::Fungible,
            100,
            10,
            5_000, // 0.005 per rank
            1_000,
        )
        .unwrap();
        let b = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::Fungible,
            100,
            10,
            5_000,
            1_000,
        )
        .unwrap();
        assert_eq!(a, b);
        // n=0 => 0
        let zero = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::Fungible,
            0,
            10,
            5_000,
            1_000,
        )
        .unwrap();
        assert_eq!(zero, 0);
        // NFT with same params now computes like fungible (virtual-rank model).
        let nft = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::NonFungible,
            100,
            10,
            5_000,
            1_000,
        )
        .unwrap();
        assert_eq!(nft, a);
    });
}

#[test]
fn per_unit_cap_never_exceeded_in_aggregate_computation() {
    new_test_ext().execute_with(|| {
        // base 10, slope 1_000_000 ppm => +1 per rank, cap 15, n=20
        // raw: 10,11,12,13,14,15,16,...
        // clamped at 15 from index 5 onward: 5 terms 10..14 sum=60, 15 terms at 15=225 => 285
        let agg = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::Fungible,
            20,
            10,
            1_000_000,
            15,
        )
        .unwrap();
        assert_eq!(agg, 285);
        // every clamped unit must be within [-cap, cap]
        for i in 0..20u128 {
            let raw = AssetWeight::compute_unit_weight_raw(10, 1_000_000, i);
            let clamped = AssetWeight::clamp_unit_weight(raw, 15);
            assert!(clamped >= -15 && clamped <= 15, "i={i} raw={raw} clamped={clamped}");
        }
    });
}

#[test]
fn dynamic_cap_stays_within_bounds() {
    new_test_ext().execute_with(|| {
        let cap = AssetWeight::update_unit_cap(1_000, 10_000_000, 10_000_000, 0);
        assert!(cap >= 1); // MinUnitCap in mock
        assert!(cap <= 1_000_000); // MaxUnitCap
        let low = AssetWeight::update_unit_cap(1, 0, 0, 10_000_000);
        assert_eq!(low, 1);
    });
}

#[test]
fn register_initializes_distribution_fields() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 1234, AssetKind::Fungible, 1_000);
        let s = AssetWeight::asset_state(10);
        assert_eq!(s.unit_cap_w, 1_000); // DefaultUnitCap
        assert_eq!(s.unit_base_w, 0);
        assert_eq!(s.unit_slope_ppm, 0);
        assert_eq!(s.dist_aggregate, 0);
        assert_eq!(s.last_inbound_cross_w, 0);
        assert_eq!(s.last_outbound_cross_w, 0);
        assert_eq!(s.effective_weight, s.base_weight);
    });
}

#[test]
fn nft_distribution_remains_inert_with_zero_params() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 1234, AssetKind::NonFungible, 5);
        let s = AssetWeight::asset_state(10);
        assert_eq!(s.unit_cap_w, 1_000); // DefaultUnitCap, same as fungible
        assert_eq!(s.dist_aggregate, 0);
        assert_eq!(s.effective_weight, s.base_weight);

        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after = AssetWeight::asset_state(10);
        assert_eq!(after.dist_aggregate, 0);
        assert_eq!(after.effective_weight, after.base_weight);
    });
}

#[test]
fn nft_distribution_contributes_when_params_enabled() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 1234, AssetKind::NonFungible, 5);
        let mut s = AssetWeight::asset_state(10);
        s.unit_base_w = 100;
        s.unit_slope_ppm = 1_000_000;
        s.unit_cap_w = 1_000;
        crate::AssetState::<Test>::insert(10u64, s);
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after = AssetWeight::asset_state(10);
        // base 100, slope 1/rank, cap 1000, n=5 => 100+101+102+103+104 = 510
        assert_eq!(after.dist_aggregate, 510);
        assert_eq!(
            after.effective_weight,
            after.base_weight.saturating_add(510)
        );
    });
}

#[test]
fn transfer_recomputes_distribution_aggregate() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 100, AssetKind::Fungible, 10);
        let mut s = AssetWeight::asset_state(10);
        s.unit_base_w = 1;
        s.unit_slope_ppm = 0;
        s.unit_cap_w = 1_000;
        crate::AssetState::<Test>::insert(10u64, s);
        assert_ok!(AssetWeight::record_fungible_transfer(
            RuntimeOrigin::signed(1),
            10,
            Some(20)
        ));
        let after = AssetWeight::asset_state(10);
        // slope 0, base 1, cap 1000, n=20 => 20
        assert_eq!(after.dist_aggregate, 20);
        // UnitDistWeightPpb is 100% in mock, so effective weight includes dist component
        assert_eq!(after.effective_weight, after.base_weight.saturating_add(20));
    });
}

#[test]
fn nft_per_unit_cap_never_exceeded() {
    new_test_ext().execute_with(|| {
        // Same affine/cap model applies to NFT virtual ranks.
        let agg = AssetWeight::compute_distribution_aggregate_o1(
            AssetKind::NonFungible,
            20,
            10,
            1_000_000,
            15,
        )
        .unwrap();
        assert_eq!(agg, 285);
        for i in 0..20u128 {
            let raw = AssetWeight::compute_unit_weight_raw(10, 1_000_000, i);
            let clamped = AssetWeight::clamp_unit_weight(raw, 15);
            assert!(clamped >= -15 && clamped <= 15, "i={i} raw={raw} clamped={clamped}");
        }
    });
}

#[test]
fn nft_transfer_recording_recomputes_distribution_without_oracle_mutation() {
    new_test_ext().execute_with(|| {
        register(10, 1, b"p", 1234, AssetKind::NonFungible, 10);
        let mut s = AssetWeight::asset_state(10);
        s.unit_base_w = 1;
        s.unit_slope_ppm = 0;
        s.unit_cap_w = 1_000;
        crate::AssetState::<Test>::insert(10u64, s);

        let before = AssetWeight::asset_state(10);
        assert_ok!(AssetWeight::record_fungible_transfer(
            RuntimeOrigin::signed(1),
            10,
            Some(20)
        ));
        let after = AssetWeight::asset_state(10);
        assert_eq!(after.last_price, before.last_price);
        assert_eq!(after.effective_price, before.effective_price);
        assert_eq!(after.transfer_count, before.transfer_count + 1);
        assert_eq!(after.total_supply, 20);
        assert_eq!(after.dist_aggregate, 20);
        assert_eq!(after.effective_weight, after.base_weight.saturating_add(20));
    });
}

#[test]
fn nft_refresh_propagates_distribution_delta_same_sign() {
    new_test_ext().execute_with(|| {
        // NFT source with dist params; target is dissimilar fungible.
        register(10, 1, b"a", 1_100, AssetKind::NonFungible, 1_050); // base 50
        register(11, 2, b"b", 1_000, AssetKind::Fungible, 1_140);
        let mut s = AssetWeight::asset_state(10);
        s.unit_base_w = 1;
        s.unit_slope_ppm = 0;
        s.unit_cap_w = 1_000;
        s.dist_aggregate = 10;
        s.effective_weight = s.base_weight.saturating_add(10);
        crate::AssetState::<Test>::insert(10u64, s);

        let target_before = AssetWeight::asset_state(11).effective_price;
        let oracle_before = AssetWeight::asset_state(10).last_price;
        assert_ok!(AssetWeight::record_fungible_transfer(
            RuntimeOrigin::signed(1),
            10,
            Some(20)
        ));
        set_price(1, b"a", Ok(1_100));
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));

        let after = AssetWeight::asset_state(10);
        let target_after = AssetWeight::asset_state(11).effective_price;
        assert_eq!(after.last_price, oracle_before);
        assert_eq!(after.dist_aggregate, 20);
        assert!(
            target_after > target_before,
            "positive NFT dist delta should propagate same-sign: {target_before}->{target_after}"
        );
    });
}

#[test]
fn distribution_contributes_to_price_movement_on_refresh() {
    new_test_ext().execute_with(|| {
        // Source has distribution params that change effective weight on refresh.
        register(10, 1, b"p", 100, AssetKind::Fungible, 10);
        register(11, 2, b"q", 1_000, AssetKind::Fungible, 1_000);
        let mut s = AssetWeight::asset_state(10);
        s.unit_base_w = 1;
        s.unit_slope_ppm = 0;
        s.unit_cap_w = 1_000;
        s.dist_aggregate = 10;
        s.effective_weight = s.base_weight.saturating_add(10);
        crate::AssetState::<Test>::insert(10u64, s);

        let before_target = AssetWeight::asset_state(11).effective_price;
        let before_oracle = AssetWeight::asset_state(10).last_price;
        // Change supply observed through transfer, then refresh: dist aggregate becomes 20.
        assert_ok!(AssetWeight::record_fungible_transfer(
            RuntimeOrigin::signed(1),
            10,
            Some(20)
        ));
        set_price(1, b"p", Ok(100));
        assert_ok!(AssetWeight::refresh_weight(RuntimeOrigin::signed(2), 10));
        let after = AssetWeight::asset_state(10);
        let after_target = AssetWeight::asset_state(11).effective_price;

        assert_eq!(after.last_price, before_oracle, "oracle last_price unchanged");
        assert_eq!(after.dist_aggregate, 20);
        assert_ne!(after.effective_weight, after.base_weight);
        assert!(
            after_target != before_target,
            "distribution-driven effective delta should propagate to target"
        );
    });
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {AMMStrategyBase} from "./AMMStrategyBase.sol";
import {TradeInfo} from "./IAMMStrategy.sol";

/// @title EWMA-Vol + Momentum + Inventory Skew v2
/// @notice Lower base to attract retail; momentum signal taxes sustained directional flow (arbs)
contract Strategy is AMMStrategyBase {
    // slots[0] = ewma_vol       EWMA of relative trade size (WAD)
    // slots[1] = reserve_ratio  X/(X+Y) post-trade (WAD)
    // slots[2] = momentum       EWMA of isBuy (1=all-buy, 0=all-sell); 0.5=neutral

    // Base fee below 30 bps normaliser → retail routes here preferentially
    uint256 constant BASE_BPS            = 12;

    // Vol EWMA: α=0.15 (same as v1)
    uint256 constant ALPHA               = 15e16;
    uint256 constant ONE_MINUS_ALPHA     = 85e16;
    uint256 constant VOL_SCALE           = 3e17;

    // Inventory skew (same as v1)
    uint256 constant SKEW_SCALE          = 1e16;

    // Large-trade surcharge: now triggers at 0.3% (was 0.5%), +15 bps (was +10)
    uint256 constant LARGE_THRESHOLD_BPS = 30;
    uint256 constant LARGE_SURCHARGE_BPS = 15;

    // Momentum surcharge: if momentum EWMA drifts past 0.65 or below 0.35,
    // sustained directional flow is detected → add 20 bps to both sides.
    // Arbs push price in one direction repeatedly; retail is random.
    uint256 constant MOMENTUM_ALPHA          = 20e16;  // faster decay so it reacts quickly
    uint256 constant MOMENTUM_ONE_MINUS      = 80e16;
    uint256 constant MOMENTUM_THRESHOLD_HIGH = 65e16;  // 0.65 WAD
    uint256 constant MOMENTUM_THRESHOLD_LOW  = 35e16;  // 0.35 WAD
    uint256 constant MOMENTUM_SURCHARGE_BPS  = 20;

    function afterInitialize(uint256 initialX, uint256 initialY) external override returns (uint256 bidFee, uint256 askFee) {
        slots[0] = 3e15;          // seed ewma_vol at 0.3%
        slots[1] = _ratio(initialX, initialY);
        slots[2] = WAD / 2;       // neutral momentum
        return (bpsToWad(BASE_BPS), bpsToWad(BASE_BPS));
    }

    function afterSwap(TradeInfo calldata trade) external override returns (uint256 bidFee, uint256 askFee) {
        uint256 rx = trade.reserveX;
        uint256 ry = trade.reserveY;
        if (rx == 0 || ry == 0) return (bpsToWad(BASE_BPS), bpsToWad(BASE_BPS));

        // Pre-trade reserveX (reserves are post-trade in TradeInfo)
        uint256 pre_rx = trade.isBuy
            ? (rx > trade.amountX ? rx - trade.amountX : 1)
            : rx + trade.amountX;

        // 1. Update EWMA vol
        uint256 rel = trade.amountX * WAD / pre_rx;
        uint256 ev  = wmul(ALPHA, rel) + wmul(ONE_MINUS_ALPHA, slots[0]);
        slots[0] = ev;

        // 2. Update inventory ratio
        uint256 ratio = _ratio(rx, ry);
        slots[1] = ratio;

        // 3. Update momentum EWMA (1 = buy, 0 = sell)
        uint256 sig = trade.isBuy ? WAD : 0;
        uint256 mom = wmul(MOMENTUM_ALPHA, sig) + wmul(MOMENTUM_ONE_MINUS, slots[2]);
        slots[2] = mom;

        // 4. Vol fee
        uint256 vol_fee = wmul(ev, VOL_SCALE);

        // 5. Large-trade surcharge (arbs trade large, retail trades small)
        uint256 lrg = (pre_rx > 0 && trade.amountX * 10_000 / pre_rx > LARGE_THRESHOLD_BPS)
            ? bpsToWad(LARGE_SURCHARGE_BPS) : 0;

        // 6. Momentum surcharge (sustained directional = arb signal)
        uint256 mev = (mom > MOMENTUM_THRESHOLD_HIGH || mom < MOMENTUM_THRESHOLD_LOW)
            ? bpsToWad(MOMENTUM_SURCHARGE_BPS) : 0;

        // 7. Symmetric base
        uint256 sym = bpsToWad(BASE_BPS) + vol_fee + lrg + mev;

        // 8. Inventory skew: lower fee on the side that rebalances pool
        uint256 half = WAD / 2;
        uint256 skew = wmul(ratio > half ? ratio - half : half - ratio, SKEW_SCALE);

        if (ratio > half) {
            // Heavy on X: raise bidFee, lower askFee
            bidFee = clampFee(sym + skew);
            askFee = clampFee(sym > skew ? sym - skew : 0);
        } else {
            // Light on X: lower bidFee, raise askFee
            bidFee = clampFee(sym > skew ? sym - skew : 0);
            askFee = clampFee(sym + skew);
        }
    }

    function getName() external pure override returns (string memory) {
        return "EWMA-Vol + Momentum + Inventory Skew v2";
    }

    function _ratio(uint256 x, uint256 y) internal pure returns (uint256) {
        uint256 t = x + y;
        return t == 0 ? WAD / 2 : x * WAD / t;
    }
}

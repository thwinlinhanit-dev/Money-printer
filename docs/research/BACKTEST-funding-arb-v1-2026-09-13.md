# funding-arb-v1 — full-cost backtest (pre-registered protocol)

Run: `farb2-backtest-2026-09-13` · corpus: FARB-2-cleared pairs (`run_backlog_event_studies.CROSS_VENUE_PAIRS`) · deterministic.

Accrual: hyperliquid hourly settlements + bybit 8h settlements (last
recorded frame at/before each 00/08/16 UTC boundary), RAW per-period
rates, cash = −side × rate; signal annualized per FARB-3; entries AND
exits act on the prior bar's signal (no lookahead); forced exits at
segment end flagged `data_end`.

| config | cost column | n | expectancy bps | win rate | windows | WF flips |
|---|---|---|---|---|---|---|
| 500entry/250exit | base | 36 | -28.132 | 0.0 | 8 | 0 |
| 1000entry/500exit | base | 22 | -28.164 | 0.0 | 6 | 0 |
| 500entry/250exit | base_2x | 36 | -57.132 | 0.0 | 8 | 0 |
| 1000entry/500exit | base_2x | 22 | -57.164 | 0.0 | 6 | 0 |

## Falsification (2x-cost column, per hypothesis)
- **500entry/250exit**: killed=True (expectancy_le_0_at_2x_cost=True, edge_in_lt_3_calendar_windows=False, wf_oos_signflip_ge_2_of_3=False)
- **1000entry/500exit**: killed=True (expectancy_le_0_at_2x_cost=True, edge_in_lt_3_calendar_windows=False, wf_oos_signflip_ge_2_of_3=False)

## Episodes (all, net of base and 2x costs)
| config | pair | entry_ts | held_h | side | entry bps/yr | exit | carry | net base | net 2x |
|---|---|---|---|---|---|---|---|---|---|
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787677200000000000 | 11 | short_A_long_B | 1116.5 | normalized | 1.439 | -27.561 | -56.561 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787677200000000000 | 2 | short_A_long_B | 1312.3 | normalized | 0.082 | -28.918 | -57.918 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787738400000000000 | 12 | short_A_long_B | 1043.6 | normalized | 1.57 | -27.43 | -56.43 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787774400000000000 | 13 | short_A_long_B | 1095.0 | normalized | 1.188 | -27.812 | -56.812 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787850000000000000 | 2 | short_A_long_B | 1322.0 | normalized | 0.119 | -28.881 | -57.881 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787875200000000000 | 9 | short_A_long_B | 1007.3 | normalized | 0.762 | -28.238 | -57.238 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787990400000000000 | 25 | short_A_long_B | 1063.1 | normalized | 2.24 | -26.76 | -55.76 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788224400000000000 | 3 | short_A_long_B | 1095.0 | normalized | 0.5 | -28.5 | -57.5 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788339600000000000 | 8 | short_A_long_B | 1259.0 | normalized | 0.853 | -28.147 | -57.147 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788656400000000000 | 38 | short_A_long_B | 1178.8 | normalized | 3.746 | -25.254 | -54.254 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788660000000000000 | 2 | short_A_long_B | 1071.0 | normalized | 0.375 | -28.625 | -57.625 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788688800000000000 | 8 | short_A_long_B | 1133.0 | normalized | 1.016 | -27.984 | -56.984 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788742800000000000 | 19 | short_A_long_B | 1069.6 | normalized | 2.115 | -26.885 | -55.885 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788807600000000000 | 1 | long_A_short_B | -1354.4 | normalized | 0.091 | -28.909 | -57.909 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788814800000000000 | 7 | long_A_short_B | -1111.6 | normalized | 0.818 | -28.182 | -57.182 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788847200000000000 | 4 | long_A_short_B | -1320.3 | normalized | 0.301 | -28.699 | -57.699 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788872400000000000 | 4 | short_A_long_B | 1095.0 | normalized | 0.138 | -28.862 | -57.862 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788951600000000000 | 6 | short_A_long_B | 1034.2 | normalized | 0.474 | -28.526 | -57.526 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788980400000000000 | 1 | short_A_long_B | 1095.0 | normalized | -0.086 | -29.086 | -58.086 |
| 1000entry/500exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788991200000000000 | 2 | long_A_short_B | -1066.0 | normalized | 0.209 | -28.791 | -57.791 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788994800000000000 | 2 | long_A_short_B | -1063.2 | normalized | -0.425 | -29.425 | -58.425 |
| 1000entry/500exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1789059600000000000 | 6 | short_A_long_B | 1375.4 | data_end | 0.872 | -28.128 | -57.128 |
| 500entry/250exit | hyperliquid-BTC-vs-binance-BTCUSDT | 1786150800000000000 | 3 | long_A_short_B | -795.5 | normalized | -0.012 | -29.012 | -58.012 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787648400000000000 | 1 | short_A_long_B | 892.5 | normalized | 0.25 | -28.75 | -57.75 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787670000000000000 | 42 | short_A_long_B | 671.7 | normalized | 3.74 | -25.26 | -54.26 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787677200000000000 | 2 | short_A_long_B | 1312.3 | normalized | 0.082 | -28.918 | -57.918 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787731200000000000 | 15 | short_A_long_B | 683.6 | normalized | 1.57 | -27.43 | -56.43 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787806800000000000 | 3 | short_A_long_B | 557.7 | data_end | 0.288 | -28.712 | -57.712 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787828400000000000 | 1 | long_A_short_B | -675.3 | normalized | -0.124 | -29.124 | -58.124 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787842800000000000 | 26 | short_A_long_B | 967.2 | normalized | 1.752 | -27.248 | -56.248 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787846400000000000 | 10 | short_A_long_B | 605.5 | normalized | 0.632 | -28.368 | -57.368 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787896800000000000 | 3 | short_A_long_B | 506.0 | normalized | -0.064 | -29.064 | -58.064 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1787943600000000000 | 45 | short_A_long_B | 649.0 | data_end | 4.265 | -24.735 | -53.735 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1787979600000000000 | 4 | short_A_long_B | 539.3 | normalized | 0.232 | -28.768 | -57.768 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788022800000000000 | 16 | short_A_long_B | 876.7 | normalized | 1.02 | -27.98 | -56.98 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788109200000000000 | 6 | short_A_long_B | 719.6 | data_end | 0.875 | -28.125 | -57.125 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788206400000000000 | 12 | short_A_long_B | 528.1 | data_end | 0.581 | -28.419 | -57.419 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788217200000000000 | 6 | short_A_long_B | 712.0 | normalized | 0.656 | -28.344 | -57.344 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788310800000000000 | 4 | short_A_long_B | 526.4 | normalized | 0.625 | -28.375 | -57.375 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788310800000000000 | 17 | short_A_long_B | 677.6 | normalized | 1.565 | -27.435 | -56.435 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788656400000000000 | 38 | short_A_long_B | 1178.8 | normalized | 3.746 | -25.254 | -54.254 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788656400000000000 | 21 | short_A_long_B | 817.2 | normalized | 2.018 | -26.982 | -55.982 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788742800000000000 | 20 | short_A_long_B | 1069.6 | normalized | 2.24 | -26.76 | -55.76 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788800400000000000 | 17 | long_A_short_B | -598.1 | normalized | 1.458 | -27.542 | -56.542 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788818400000000000 | 12 | short_A_long_B | 508.2 | normalized | 1.081 | -27.919 | -56.919 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788865200000000000 | 1 | short_A_long_B | 623.4 | normalized | 0.129 | -28.871 | -57.871 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788865200000000000 | 6 | short_A_long_B | 528.4 | normalized | 0.388 | -28.612 | -57.612 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788876000000000000 | 3 | short_A_long_B | 665.5 | normalized | -0.079 | -29.079 | -58.079 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788897600000000000 | 5 | short_A_long_B | 600.1 | normalized | 0.411 | -28.589 | -57.589 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788926400000000000 | 5 | short_A_long_B | 559.4 | normalized | 0.167 | -28.833 | -57.833 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788948000000000000 | 10 | short_A_long_B | 576.4 | normalized | 0.638 | -28.362 | -57.362 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788955200000000000 | 4 | short_A_long_B | 537.0 | normalized | -0.147 | -29.147 | -58.147 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1788976800000000000 | 7 | short_A_long_B | 662.0 | normalized | -0.192 | -29.192 | -58.192 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1788991200000000000 | 10 | long_A_short_B | -731.7 | normalized | -0.4 | -29.4 | -58.4 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1789027200000000000 | 2 | long_A_short_B | -569.5 | normalized | 0.19 | -28.81 | -57.81 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1789030800000000000 | 2 | short_A_long_B | 649.7 | normalized | -0.084 | -29.084 | -58.084 |
| 500entry/250exit | hyperliquid-ETH-vs-bybit-ETHUSDT | 1789041600000000000 | 11 | short_A_long_B | 600.8 | data_end | 1.514 | -27.486 | -56.486 |
| 500entry/250exit | hyperliquid-BTC-vs-bybit-BTCUSDT | 1789059600000000000 | 1 | short_A_long_B | 573.0 | normalized | 0.25 | -28.75 | -57.75 |

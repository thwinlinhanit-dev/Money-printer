#!/usr/bin/env python3
"""Download BTC daily OHLCV data from CoinGecko API"""

import json
import urllib.request
from datetime import datetime, timedelta
from pathlib import Path

def download_btc_daily():
    """Download BTC daily data from CoinGecko"""
    
    # CoinGecko free API - 365 days max per call
    # We need ~3000 days (8+ years)
    
    all_bars = []
    
    # Calculate date ranges (CoinGecko allows max 365 days per call)
    end_date = datetime(2026, 8, 28)
    start_date = datetime(2017, 1, 1)
    
    current = start_date
    while current < end_date:
        range_end = min(current + timedelta(days=365), end_date)
        
        # Unix timestamps
        from_ts = int(current.timestamp())
        to_ts = int(range_end.timestamp())
        
        url = f"https://api.coingecko.com/api/v3/coins/bitcoin/market_chart/range?vs_currency=usd&from={from_ts}&to={to_ts}"
        
        print(f"Fetching {current.date()} to {range_end.date()}...")
        
        try:
            req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads(resp.read())
                
                prices = data.get('prices', [])
                volumes = data.get('total_volumes', [])
                
                # Combine into OHLCV (CoinGecko only gives close + volume)
                for i, (price_point, vol_point) in enumerate(zip(prices, volumes)):
                    ts = price_point[0] / 1000  # Convert ms to seconds
                    close = price_point[1]
                    volume = vol_point[1]
                    
                    # CoinGecko doesn't give OHLC, just close
                    # We'll use close as proxy (acceptable for pattern detection)
                    all_bars.append({
                        "time": int(ts),
                        "open": close,  # Placeholder
                        "high": close,  # Placeholder
                        "low": close,   # Placeholder
                        "close": close,
                        "volume": volume
                    })
                
                print(f"  Got {len(prices)} bars")
                
        except Exception as e:
            print(f"  Error: {e}")
        
        current = range_end + timedelta(days=1)
        
        # Rate limit
        import time
        time.sleep(6)
    
    # Deduplicate by date
    seen_dates = set()
    unique_bars = []
    for bar in all_bars:
        date_str = datetime.fromtimestamp(bar['time']).strftime('%Y-%m-%d')
        if date_str not in seen_dates:
            seen_dates.add(date_str)
            unique_bars.append(bar)
    
    # Sort by time
    unique_bars.sort(key=lambda x: x['time'])
    
    print(f"\nTotal unique bars: {len(unique_bars)}")
    
    # Save
    out_path = Path("research/out/btc_daily_bars.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, 'w') as f:
        json.dump(unique_bars, f, indent=2)
    
    print(f"Saved to {out_path}")
    
    # Show sample
    print("\nSample bars:")
    for bar in unique_bars[-5:]:
        dt = datetime.fromtimestamp(bar['time']).strftime('%Y-%m-%d')
        print(f"{dt} C: ${bar['close']:,.0f}")

if __name__ == "__main__":
    download_btc_daily()

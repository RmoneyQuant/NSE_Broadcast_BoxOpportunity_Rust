def calculate_box_sell_price(quotes, k1, k2):
    """
    Box Sell formula:

    -Call(K1) + Put(K1) + Call(K2) - Put(K2)

    Execution logic:

    -Call(K1)  means sell Call(K1) -> use bid
    +Put(K1)   means buy Put(K1)   -> use ask
    +Call(K2)  means buy Call(K2)  -> use ask
    -Put(K2)   means sell Put(K2)  -> use bid
    """

    c1 = quotes[(k1, "CE")]
    p1 = quotes[(k1, "PE")]
    c2 = quotes[(k2, "CE")]
    p2 = quotes[(k2, "PE")]

    zero_legs = []
    if not c1["bid"]: zero_legs.append(f"CE({k1}) bid=0")
    if not p1["ask"]: zero_legs.append(f"PE({k1}) ask=0")
    if not c2["ask"]: zero_legs.append(f"CE({k2}) ask=0")
    if not p2["bid"]: zero_legs.append(f"PE({k2}) bid=0")
    if zero_legs:
        raise ValueError(f"Zero price leg(s) in BOX SELL ({k1}/{k2}): {', '.join(zero_legs)}")

    box_sell_price = (
        c1["bid"]
        - p1["ask"]
        - c2["ask"]
        + p2["bid"]
    )

    legs = {
        "sell_call_k1": {
            "contract": c1["display_name"],
            "price_used": c1["bid"]
        },
        "buy_put_k1": {
            "contract": p1["display_name"],
            "price_used": p1["ask"]
        },
        "buy_call_k2": {
            "contract": c2["display_name"],
            "price_used": c2["ask"]
        },
        "sell_put_k2": {
            "contract": p2["display_name"],
            "price_used": p2["bid"]
        }
    }

    return box_sell_price, legs

def calculate_box_long_price(quotes, k1, k2):
    """
    Box Long formula:

    +Call(K1) - Put(K1) - Call(K2) + Put(K2)

    Executable price:

    B_buy = C(K1)_ask - P(K1)_bid - C(K2)_bid + P(K2)_ask
    """

    c1 = quotes[(k1, "CE")]
    p1 = quotes[(k1, "PE")]
    c2 = quotes[(k2, "CE")]
    p2 = quotes[(k2, "PE")]

    zero_legs = []
    if not c1["ask"]: zero_legs.append(f"CE({k1}) ask=0")
    if not p1["bid"]: zero_legs.append(f"PE({k1}) bid=0")
    if not c2["bid"]: zero_legs.append(f"CE({k2}) bid=0")
    if not p2["ask"]: zero_legs.append(f"PE({k2}) ask=0")
    if zero_legs:
        raise ValueError(f"Zero price leg(s) in BOX LONG ({k1}/{k2}): {', '.join(zero_legs)}")

    box_long_price = (
        c1["ask"]      # buy Call K1
        - p1["bid"]    # sell Put K1
        - c2["bid"]    # sell Call K2
        + p2["ask"]    # buy Put K2
    )

    legs = {
        "buy_call_k1": {
            "contract": c1["display_name"],
            "price_used": c1["ask"]
        },
        "sell_put_k1": {
            "contract": p1["display_name"],
            "price_used": p1["bid"]
        },
        "sell_call_k2": {
            "contract": c2["display_name"],
            "price_used": c2["bid"]
        },
        "buy_put_k2": {
            "contract": p2["display_name"],
            "price_used": p2["ask"]
        }
    }

    return box_long_price, legs
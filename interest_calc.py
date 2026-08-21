from datetime import datetime, date


def calculate_interest_rate(box_price, k1, k2, expiry: str):
    """expiry: DDMMMYYYY format, e.g. '28JUL2026'"""
    expiry_date = datetime.strptime(expiry.upper(), "%d%b%Y").date()
    days_to_expiry = (expiry_date - date.today()).days

    strike_difference = k2 - k1
    ratio = box_price / strike_difference

    # On (or past) expiry day there's no time left to annualize over, and a
    # zero box price has no meaningful return — leave it blank instead of
    # raising, so the raw/net box prices for the pair still show up.
    if days_to_expiry <= 0 or box_price == 0:
        interest_calculation = None
    else:
        interest_calculation = round(((strike_difference - box_price) / box_price) / days_to_expiry * 365*100, 2)

    return {
        "strike_difference": strike_difference,
        "box_price": box_price,
        "days_to_expiry": days_to_expiry,
        "ratio_calculation": ratio,
        "interest_calculation": interest_calculation
    }

//! Pure scoring functions. Coefficients were fitted offline on the supplied datasets; see
//! `docs/DATASET_EVALUATION.md` for provenance, accuracy and limits.

// ───────────── Counterparty credit score ─────────────
// Logistic regression for "bankrupt" fitted on Enterprise_Financial_Network_Dataset nodes
// (n = 2934, 14.2% bankrupt). 5-fold CV AUC 0.719. `legal_cases` was tested and dropped:
// its fitted sign was perverse (more cases => lower risk) and it added no AUC.
pub const CP_INTERCEPT: f64 = 0.42303598485685656;
pub const CP_CREDIT: f64 = -0.004598726686763937;
pub const CP_DEBT: f64 = 2.5086421601903055;
pub const CP_LIQUIDITY: f64 = -0.5898892171646065;

/// Domain the model was fitted on; inputs outside it are flagged as extrapolated.
pub const CP_CREDIT_RANGE: (f64, f64) = (500.0, 849.0);
pub const CP_DEBT_RANGE: (f64, f64) = (0.1, 1.0);
pub const CP_LIQUIDITY_RANGE: (f64, f64) = (0.5, 2.0);

#[derive(Debug, Clone, Copy)]
pub struct CounterpartyInputs {
    pub credit_score: f64,
    pub debt_ratio: f64,
    pub liquidity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterpartyScore {
    /// Model output in [0, 1]. Relative risk: it is calibrated to the training population's
    /// 14% base rate, so it is not a true default probability for your customers.
    pub probability: f64,
    pub band: &'static str,
    pub extrapolated: bool,
}

pub fn counterparty_band(p: f64) -> &'static str {
    // Observed bankruptcy rate in cross-validation: low 5.5%, medium 16%, high 28%.
    if p < 0.10 {
        "low"
    } else if p < 0.20 {
        "medium"
    } else {
        "high"
    }
}

pub fn counterparty_score(i: CounterpartyInputs) -> CounterpartyScore {
    let z = CP_INTERCEPT + CP_CREDIT * i.credit_score + CP_DEBT * i.debt_ratio + CP_LIQUIDITY * i.liquidity;
    let p = 1.0 / (1.0 + (-z).exp());
    let out = |v: f64, r: (f64, f64)| v < r.0 || v > r.1;
    CounterpartyScore {
        probability: p,
        band: counterparty_band(p),
        extrapolated: out(i.credit_score, CP_CREDIT_RANGE)
            || out(i.debt_ratio, CP_DEBT_RANGE)
            || out(i.liquidity, CP_LIQUIDITY_RANGE),
    }
}

// ───────────── Financial health score ─────────────
// Linear fit of Financial_Risk_Score on four ratios the ledger can produce, from
// Entrepreneurship_Risk_Assessment_Dataset (n = 2500). R^2 = 0.89, residual SD 2.4 points.
// (The dataset's own score is exactly linear in 11 inputs, including funding gap and credit
// score, which an ERP does not hold; this is the ledger-computable part of it.)
pub const HS_INTERCEPT: f64 = 80.3699;
pub const HS_DEBT_PCT: f64 = 0.23648;
pub const HS_CURRENT_RATIO: f64 = -3.79491;
pub const HS_MARGIN_PCT: f64 = -0.34332;
pub const HS_RUNWAY: f64 = -0.29512;

pub const HS_DEBT_RANGE: (f64, f64) = (0.0, 100.0);
pub const HS_CR_RANGE: (f64, f64) = (0.25, 4.0);
pub const HS_MARGIN_RANGE: (f64, f64) = (-25.0, 40.0);
pub const HS_RUNWAY_RANGE: (f64, f64) = (3.0, 48.0);

#[derive(Debug, Clone, Copy)]
pub struct HealthInputs {
    pub debt_ratio_pct: f64,
    pub current_ratio: f64,
    pub profit_margin_pct: f64,
    pub runway_months: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HealthScore {
    /// 0-100, higher = riskier. Relative to the benchmark population (median 69, IQR 64-74).
    pub score: f64,
    pub band: &'static str,
    /// Inputs that fell outside the fitted range and were clamped before scoring.
    pub clamped: Vec<&'static str>,
}

pub fn health_band(score: f64) -> &'static str {
    // 64 and 74 are the benchmark population's 25th and 75th percentiles.
    if score < 64.0 {
        "low"
    } else if score <= 74.0 {
        "elevated"
    } else {
        "high"
    }
}

fn clamp_tracked(v: f64, r: (f64, f64), name: &'static str, log: &mut Vec<&'static str>) -> f64 {
    if v < r.0 || v > r.1 {
        log.push(name);
    }
    v.clamp(r.0, r.1)
}

pub fn health_score(i: HealthInputs) -> HealthScore {
    let mut clamped = vec![];
    let d = clamp_tracked(i.debt_ratio_pct, HS_DEBT_RANGE, "debt_ratio_pct", &mut clamped);
    let c = clamp_tracked(i.current_ratio, HS_CR_RANGE, "current_ratio", &mut clamped);
    let m = clamp_tracked(i.profit_margin_pct, HS_MARGIN_RANGE, "profit_margin_pct", &mut clamped);
    let r = clamp_tracked(i.runway_months, HS_RUNWAY_RANGE, "runway_months", &mut clamped);
    let score = (HS_INTERCEPT + HS_DEBT_PCT * d + HS_CURRENT_RATIO * c + HS_MARGIN_PCT * m + HS_RUNWAY * r)
        .clamp(0.0, 100.0);
    HealthScore { score, band: health_band(score), clamped }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_counterparty_is_low_and_weak_is_high() {
        let good = counterparty_score(CounterpartyInputs { credit_score: 800.0, debt_ratio: 0.2, liquidity: 1.8 });
        let bad = counterparty_score(CounterpartyInputs { credit_score: 520.0, debt_ratio: 0.9, liquidity: 0.6 });
        assert_eq!(good.band, "low");
        assert_eq!(bad.band, "high");
        assert!(good.probability < 0.05 && bad.probability > 0.4);
        assert!(!good.extrapolated);
    }

    #[test]
    fn counterparty_score_is_monotonic_in_each_input() {
        let base = CounterpartyInputs { credit_score: 650.0, debt_ratio: 0.5, liquidity: 1.2 };
        let p = |i| counterparty_score(i).probability;
        assert!(p(CounterpartyInputs { credit_score: 700.0, ..base }) < p(base));
        assert!(p(CounterpartyInputs { debt_ratio: 0.7, ..base }) > p(base));
        assert!(p(CounterpartyInputs { liquidity: 1.6, ..base }) < p(base));
    }

    #[test]
    fn out_of_domain_counterparty_is_flagged() {
        let s = counterparty_score(CounterpartyInputs { credit_score: 350.0, debt_ratio: 1.4, liquidity: 0.2 });
        assert!(s.extrapolated);
        assert!(s.probability > 0.0 && s.probability < 1.0);
    }

    #[test]
    fn health_score_reference_points() {
        let healthy = health_score(HealthInputs { debt_ratio_pct: 20.0, current_ratio: 3.0, profit_margin_pct: 20.0, runway_months: 48.0 });
        let typical = health_score(HealthInputs { debt_ratio_pct: 40.0, current_ratio: 1.5, profit_margin_pct: 10.0, runway_months: 48.0 });
        let stressed = health_score(HealthInputs { debt_ratio_pct: 80.0, current_ratio: 0.6, profit_margin_pct: -10.0, runway_months: 5.0 });
        assert_eq!((healthy.band, typical.band, stressed.band), ("low", "elevated", "high"));
        assert!((typical.score - 66.5).abs() < 0.1);
    }

    #[test]
    fn health_inputs_are_clamped_and_reported() {
        let s = health_score(HealthInputs { debt_ratio_pct: 250.0, current_ratio: 0.0, profit_margin_pct: -900.0, runway_months: 0.0 });
        assert_eq!(s.clamped, vec!["debt_ratio_pct", "current_ratio", "profit_margin_pct", "runway_months"]);
        assert!(s.score <= 100.0 && s.score >= 0.0);
    }
}

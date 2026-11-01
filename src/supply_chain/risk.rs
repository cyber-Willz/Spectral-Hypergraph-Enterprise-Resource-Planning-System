//! Supplier risk and performance scoring.
//!
//! Structural risk: logistic model fitted on the 24,112-supplier Supplier Risk Assessment dataset
//! (5-fold CV accuracy 0.92, AUC 0.97). Inputs are supplier attributes; missing inputs are imputed
//! with the dataset median and reported.
//! Performance: observed on-time / in-full / quality results. The datasets show the two are only
//! weakly related (labelled risk did not predict on-time delivery in the procurement data), so both
//! are reported side by side instead of being merged into one number.

pub struct RiskInputs {
    pub financial_stability: Option<f64>,
    pub geopolitical_risk: Option<f64>,
    pub alternatives: Option<f64>,
    pub environmental_compliance: Option<f64>,
    pub previous_disruptions: Option<f64>,
}

const INTERCEPT: f64 = 18.6731;
// (coefficient, dataset median used for imputation)
const FIN: (f64, f64) = (-0.28082, 56.6);
const GEO: (f64, f64) = (0.19887, 31.0);
const ALT: (f64, f64) = (-0.94107, 3.0);
const ENV: (f64, f64) = (-0.07604, 71.48);
const DIS: (f64, f64) = (1.43018, 1.0);

/// Returns (probability of high-risk flag, number of imputed inputs).
pub fn risk_probability(i: &RiskInputs) -> (f64, usize) {
    let mut missing = 0;
    let mut term = |v: Option<f64>, (w, med): (f64, f64)| {
        w * v.unwrap_or_else(|| {
            missing += 1;
            med
        })
    };
    let z = INTERCEPT
        + term(i.financial_stability, FIN)
        + term(i.geopolitical_risk, GEO)
        + term(i.alternatives, ALT)
        + term(i.environmental_compliance, ENV)
        + term(i.previous_disruptions, DIS);
    (1.0 / (1.0 + (-z).exp()), missing)
}

pub fn risk_band(p: f64) -> &'static str {
    if p < 0.30 {
        "low"
    } else if p < 0.70 {
        "medium"
    } else {
        "high"
    }
}

/// Weighted incident load per 100 received PO lines (minor 1, major 3, critical 5).
pub fn quality_score(minor: i64, major: i64, critical: i64, lines: i64) -> f64 {
    if lines <= 0 {
        return 100.0;
    }
    let weighted = (minor + 3 * major + 5 * critical) as f64 / lines as f64 * 100.0;
    100.0 * (1.0 - (weighted / 10.0).min(1.0))
}

/// 40% on-time, 30% in-full, 30% quality (all on a 0-100 scale).
pub fn performance_score(on_time: f64, in_full: f64, quality: f64) -> f64 {
    0.4 * on_time + 0.3 * in_full + 0.3 * quality
}

pub fn recommendation(perf: Option<f64>, band: &str) -> &'static str {
    match perf {
        Some(p) if p < 60.0 => "review",
        Some(p) if p < 75.0 => "monitor",
        _ if band == "high" => "monitor",
        _ => "ok",
    }
}

// ---- shipment delay model ---------------------------------------------------------------------------------
//
// Logistic model fitted on the 100,000-shipment Supply Chain Operation & Delivery Performance dataset
// (25.3% delayed). Held-out (20%) AUC 0.904, Brier 0.105 vs 0.188 for the base rate, and predicted
// delay rates match observed rates in every quintile (0.7/3.7/12.7/35.1/75.6% predicted vs
// 0.4/2.9/12.2/35.6/74.6% observed). Mode, priority, distance, customs flags and documentation errors
// carried no signal in that data and are deliberately not inputs.

pub const WEATHER_DELAY: [(&str, f64); 8] = [
    ("clear", 0.0),
    ("cloudy", 0.14937),
    ("extreme heat", 0.30443),
    ("fog", 0.5218),
    ("heavy rain", 0.61918),
    ("rain", 0.27099),
    ("snow", 0.81966),
    ("storm", 1.1761),
];

pub struct DelayInputs {
    /// 10-100
    pub traffic_index: Option<f64>,
    /// percent, 0-100
    pub supplier_reliability: Option<f64>,
    /// percent, 0-100
    pub carrier_reliability: Option<f64>,
    /// percent of capacity in use, 0-100
    pub warehouse_utilization: Option<f64>,
    pub weather: Option<String>,
}

/// Canonical weather label or None when unknown. "hurricane" is treated as "storm" here.
pub fn normalise_weather(w: &str) -> Option<&'static str> {
    let w = w.trim().to_ascii_lowercase();
    let w = if w == "hurricane" { "storm".to_string() } else { w };
    WEATHER_DELAY.iter().find(|(n, _)| *n == w).map(|(n, _)| *n)
}

/// P(delivery delayed) and the number of inputs that were imputed with dataset medians.
pub fn delay_probability(i: &DelayInputs) -> (f64, usize) {
    let mut imputed = 0;
    let mut get = |v: Option<f64>, med: f64| {
        v.unwrap_or_else(|| {
            imputed += 1;
            med
        })
    };
    let traffic = get(i.traffic_index, 54.0);
    let sup = get(i.supplier_reliability, 77.8);
    let car = get(i.carrier_reliability, 81.4);
    let util = get(i.warehouse_utilization, 74.2);
    let wx = match i.weather.as_deref().and_then(normalise_weather) {
        Some(n) => WEATHER_DELAY.iter().find(|(m, _)| *m == n).map(|(_, c)| *c).unwrap_or(0.0),
        None => {
            imputed += 1;
            0.0
        }
    };
    let z = 4.20184 + 0.07069 * traffic - 0.08809 * sup - 0.08873 * car + 0.05193 * util + wx;
    (1.0 / (1.0 + (-z).exp()), imputed)
}

// ---- international disruption model ------------------------------------------------------------------------
//
// Logistic model on the 5,000-shipment Global Supply Chain Risk dataset (61% disrupted). Held-out AUC 0.835;
// hurricanes disrupted 100% of shipments, storms 80%, fog 48%, rain 42%, clear 37%. Transport mode, distance,
// fuel price, weight and product category showed no effect.

pub struct DisruptionInputs {
    /// 0-10
    pub geopolitical_risk: Option<f64>,
    /// percent, 0-100
    pub carrier_reliability: Option<f64>,
    pub weather: Option<String>,
}

/// Maps the shared weather vocabulary onto the five levels present in the disruption data.
pub fn disruption_weather(w: &str) -> Option<(&'static str, f64)> {
    let w = w.trim().to_ascii_lowercase();
    match w.as_str() {
        "clear" | "cloudy" | "extreme heat" => Some(("clear", 0.0)),
        "rain" | "heavy rain" => Some(("rain", 0.16209)),
        "fog" | "snow" => Some(("fog", 0.44968)),
        "storm" => Some(("storm", 2.04522)),
        "hurricane" => Some(("hurricane", 6.42036)),
        _ => None,
    }
}

pub fn disruption_probability(i: &DisruptionInputs) -> (f64, usize) {
    let mut imputed = 0;
    let geo = i.geopolitical_risk.unwrap_or_else(|| {
        imputed += 1;
        5.1
    });
    let car = i.carrier_reliability.map(|c| c / 100.0).unwrap_or_else(|| {
        imputed += 1;
        0.757
    });
    let wx = match i.weather.as_deref().and_then(disruption_weather) {
        Some((_, c)) => c,
        None => {
            imputed += 1;
            0.0
        }
    };
    let z = -0.61132 + 0.23095 * geo - 1.47138 * car + wx;
    (1.0 / (1.0 + (-z).exp()), imputed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_supplier_is_low_risk() {
        let (p, m) = risk_probability(&RiskInputs {
            financial_stability: Some(85.0),
            geopolitical_risk: Some(15.0),
            alternatives: Some(6.0),
            environmental_compliance: Some(90.0),
            previous_disruptions: Some(0.0),
        });
        assert_eq!(m, 0);
        assert!(p < 0.30, "{p}");
    }

    #[test]
    fn weak_supplier_is_high_risk_and_imputation_counted() {
        let (p, m) = risk_probability(&RiskInputs {
            financial_stability: Some(35.0),
            geopolitical_risk: Some(70.0),
            alternatives: None,
            environmental_compliance: None,
            previous_disruptions: Some(3.0),
        });
        assert_eq!(m, 2);
        assert!(p > 0.9, "{p}");
        assert_eq!(risk_band(p), "high");
    }

    #[test]
    fn no_inputs_means_median_profile() {
        let (p, m) = risk_probability(&RiskInputs {
            financial_stability: None,
            geopolitical_risk: None,
            alternatives: None,
            environmental_compliance: None,
            previous_disruptions: None,
        });
        assert_eq!(m, 5);
        assert!(p > 0.8); // callers must treat >=3 imputed inputs as "unknown", not "high"
    }

    #[test]
    fn delay_model_reproduces_the_fitted_probabilities() {
        // three real rows from the training data (low / mid / high predicted risk); probabilities from
        // the unrounded scikit-learn fit, which the stored coefficients reproduce to 4 decimals
        let row = |t, s, c, u, w: &str| delay_probability(&DelayInputs { traffic_index: Some(t), supplier_reliability: Some(s), carrier_reliability: Some(c), warehouse_utilization: Some(u), weather: Some(w.into()) });
        let (a, ia) = row(25.0, 59.6, 99.6, 76.4, "Clear");
        let (b, _) = row(72.0, 68.0, 94.4, 79.4, "Rain");
        let (c, _) = row(54.0, 56.7, 71.8, 99.0, "Heavy Rain");
        assert_eq!(ia, 0);
        assert!((a - 0.0155).abs() < 0.003, "{a}");
        assert!((b - 0.3362).abs() < 0.01, "{b}");
        assert!((c - 0.9179).abs() < 0.01, "{c}");
        // the all-median, clear-weather profile is a low-risk shipment (~10%): the 25% base rate comes from
        // the spread of inputs, not the median case. Storms/traffic/utilisation raise the risk sharply.
        let (base, imputed) = delay_probability(&DelayInputs { traffic_index: None, supplier_reliability: None, carrier_reliability: None, warehouse_utilization: None, weather: None });
        assert_eq!(imputed, 5);
        assert!(base > 0.05 && base < 0.2, "{base}");
        let (worse, _) = delay_probability(&DelayInputs { traffic_index: Some(90.0), supplier_reliability: Some(60.0), carrier_reliability: Some(60.0), warehouse_utilization: Some(95.0), weather: Some("storm".into()) });
        assert!(worse > 0.9, "{worse}");
        assert_eq!(normalise_weather("Hurricane"), Some("storm"));
        assert_eq!(normalise_weather("meteor"), None);
    }

    #[test]
    fn disruption_model_orders_weather_and_geopolitics() {
        let p = |w: &str, g: f64| disruption_probability(&DisruptionInputs { geopolitical_risk: Some(g), carrier_reliability: Some(75.7), weather: Some(w.into()) }).0;
        assert!((p("clear", 5.1) - 0.3665).abs() < 0.01);
        assert!(p("hurricane", 5.0) > 0.99);
        assert!(p("storm", 5.0) > p("fog", 5.0) && p("fog", 5.0) > p("clear", 5.0));
        assert!(p("clear", 9.0) > p("clear", 1.0));
        let (_, imputed) = disruption_probability(&DisruptionInputs { geopolitical_risk: None, carrier_reliability: None, weather: None });
        assert_eq!(imputed, 3);
    }

    #[test]
    fn scoring() {
        // aerospace profile: 43% on time, 89% in full, 368 incidents (243/91/34) over 29,666 PO lines
        let q = quality_score(243, 91, 34, 29_666);
        assert!((q - 76.9).abs() < 0.2, "{q}");
        let perf = performance_score(43.0, 89.0, q);
        assert!((perf - 67.0).abs() < 0.3, "{perf}");
        assert_eq!(recommendation(Some(perf), "low"), "monitor");
        assert_eq!(recommendation(Some(50.0), "low"), "review");
        assert_eq!(recommendation(None, "high"), "monitor");
    }
}

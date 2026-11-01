//! Inventory-planning maths (pure functions, unit-tested).

/// Inverse standard-normal CDF (Acklam's rational approximation, |error| < 1.2e-9).
#[allow(clippy::excessive_precision)]
pub fn norm_inv(p: f64) -> f64 {
    let p = p.clamp(1e-9, 1.0 - 1e-9);
    const A: [f64; 6] = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.383577518672690e2, -3.066479806614716e1, 2.506628277459239];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let plow = 0.02425;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - plow {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -norm_inv(1.0 - p)
    }
}

/// Target cycle service level.
/// * With a stock-out cost: newsvendor critical ratio Cu / (Cu + Co), where Co is the holding cost of
///   one unit over the protection interval.
/// * Otherwise the default for the criticality class (A 99%, B 97%, C 95%).
pub fn service_level(
    criticality: &str,
    stockout_cost: Option<f64>,
    unit_cost: f64,
    holding_rate: f64,
    interval_days: f64,
) -> f64 {
    match stockout_cost {
        Some(cu) if cu > 0.0 => {
            let co = (unit_cost * holding_rate * interval_days / 365.0).max(1e-6);
            (cu / (cu + co)).clamp(0.5, 0.9999)
        }
        _ => match criticality {
            "A" => 0.99,
            "B" => 0.97,
            _ => 0.95,
        },
    }
}

/// Safety stock for demand and lead-time uncertainty over protection interval T = lead + review:
/// z * sqrt(T * sd_d^2 + mu_d^2 * sd_L^2).
pub fn safety_stock(z: f64, mu_d: f64, sd_d: f64, lead: f64, lead_sd: f64, review: f64) -> f64 {
    let t = lead + review;
    z * (t * sd_d * sd_d + mu_d * mu_d * lead_sd * lead_sd).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_normal_known_points() {
        assert!((norm_inv(0.5)).abs() < 1e-9);
        assert!((norm_inv(0.95) - 1.644854).abs() < 1e-5);
        assert!((norm_inv(0.99) - 2.326348).abs() < 1e-5);
        assert!((norm_inv(0.05) + 1.644854).abs() < 1e-5);
    }

    #[test]
    fn service_levels() {
        assert_eq!(service_level("A", None, 10.0, 0.25, 21.0), 0.99);
        assert_eq!(service_level("C", None, 10.0, 0.25, 21.0), 0.95);
        // defence data: stock-out penalties dwarf holding cost -> near-certain service
        let sl = service_level("C", Some(69_000.0), 100.0, 0.25, 7.0);
        assert!(sl > 0.999);
    }

    #[test]
    fn lead_time_variability_dominates_for_fast_movers() {
        // High-dimensional dataset profile: mu=20/day, sd=9, lead=8d, sd_L=3.8d
        let with_var = safety_stock(1.645, 20.0, 9.0, 8.0, 3.8, 0.0);
        let without = safety_stock(1.645, 20.0, 9.0, 8.0, 0.0, 0.0);
        assert!(with_var > 2.0 * without, "{with_var} vs {without}");
    }
}

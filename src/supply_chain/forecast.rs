//! Demand forecasting engine (pure functions, unit-tested).
//!
//! Methods (all produce flat one-step-ahead forecasts on a weekly series):
//! * `ma13`    13-week moving average
//! * `ses`     simple exponential smoothing
//! * `sba`     Croston with the Syntetos-Boylan bias correction (size / interval, x (1 - alpha/2))
//! * `tsb`     Teunter-Syntetos-Babai: smooths demand *probability* every period, so forecasts decay
//!   when demand stops (Croston-type methods do not)
//! * `ses_tsb` mean of `ses` and `tsb`
//! * `auto`    the method with the lowest RMSE over a rolling one-step-ahead backtest
//!   (RMSE rewards the conditional mean; WAPE/MAE reward forecasting zero on sparse items)
//!
//! Smoothing parameters are fitted by one-step SSE on the training part of the series only, then
//! frozen for the backtest window, so backtest errors are out-of-sample.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Ma13,
    Ses,
    Sba,
    Tsb,
    SesTsb,
}

impl Method {
    pub const ALL: [Method; 5] = [Method::Ma13, Method::Ses, Method::Sba, Method::Tsb, Method::SesTsb];
    pub fn name(self) -> &'static str {
        match self {
            Method::Ma13 => "ma13",
            Method::Ses => "ses",
            Method::Sba => "sba",
            Method::Tsb => "tsb",
            Method::SesTsb => "ses_tsb",
        }
    }
    pub fn parse(s: &str) -> Option<Method> {
        Method::ALL.iter().copied().find(|m| m.name() == s)
    }
}

/// Smoothing parameters (alpha, beta); beta only used by TSB.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Params {
    pub alpha: f64,
    pub beta: f64,
}

/// f[t] = forecast for period t made with data before t (f[0] = 0); f[n] is the next-period forecast.
pub fn forecasts(method: Method, p: Params, y: &[f64]) -> Vec<f64> {
    let n = y.len();
    let mut f = vec![0.0; n + 1];
    match method {
        Method::Ma13 => {
            for t in 1..=n {
                let lo = t.saturating_sub(13);
                f[t] = y[lo..t].iter().sum::<f64>() / (t - lo) as f64;
            }
        }
        Method::Ses => {
            let mut l = None::<f64>;
            for t in 0..n {
                l = Some(match l {
                    None => y[t],
                    Some(prev) => prev + p.alpha * (y[t] - prev),
                });
                f[t + 1] = l.unwrap_or(0.0);
            }
        }
        Method::Sba => {
            let (mut z, mut per, mut q) = (0.0, 1.0, 1.0);
            let mut started = false;
            for t in 0..n {
                if y[t] > 0.0 {
                    if !started {
                        z = y[t];
                        per = (t + 1) as f64;
                        started = true;
                    } else {
                        z += p.alpha * (y[t] - z);
                        per += p.alpha * (q - per);
                    }
                    q = 1.0;
                } else {
                    q += 1.0;
                }
                f[t + 1] = if started { z / per * (1.0 - p.alpha / 2.0) } else { 0.0 };
            }
        }
        Method::Tsb => {
            let warm = n.clamp(1, 13);
            let mut prob = y[..warm].iter().filter(|v| **v > 0.0).count() as f64 / warm as f64;
            let mut z = y.iter().copied().find(|v| *v > 0.0).unwrap_or(0.0);
            for t in 0..n {
                if y[t] > 0.0 {
                    prob += p.beta * (1.0 - prob);
                    z += p.alpha * (y[t] - z);
                } else {
                    prob -= p.beta * prob;
                }
                f[t + 1] = prob * z;
            }
        }
        Method::SesTsb => {
            let a = forecasts(Method::Ses, p, y);
            let b = forecasts(Method::Tsb, p, y);
            for t in 0..=n {
                f[t] = 0.5 * (a[t] + b[t]);
            }
        }
    }
    f
}

fn sse(f: &[f64], y: &[f64], from: usize, to: usize) -> f64 {
    (from..to).map(|t| (f[t] - y[t]).powi(2)).sum()
}

/// Grid-search smoothing parameters on y[..train_end].
pub fn fit(method: Method, y: &[f64], train_end: usize) -> Params {
    let warm = 4.min(train_end);
    let (alphas, betas): (&[f64], &[f64]) = match method {
        Method::Ma13 => (&[0.0], &[0.0]),
        Method::Ses => (&[0.05, 0.1, 0.2, 0.3, 0.5], &[0.0]),
        Method::Sba => (&[0.05, 0.1, 0.2, 0.3], &[0.0]),
        Method::Tsb | Method::SesTsb => (&[0.05, 0.1, 0.2, 0.3], &[0.02, 0.05, 0.1, 0.2]),
    };
    let mut best = (f64::INFINITY, Params { alpha: alphas[0], beta: betas[0] });
    for &alpha in alphas {
        for &beta in betas {
            let p = Params { alpha, beta };
            let f = forecasts(method, p, &y[..train_end]);
            let e = sse(&f, y, warm, train_end);
            if e < best.0 {
                best = (e, p);
            }
        }
    }
    best.1
}

#[derive(Clone, Debug, Serialize)]
pub struct Backtest {
    pub method: Method,
    pub alpha: f64,
    pub beta: f64,
    pub test_weeks: usize,
    pub actual: f64,
    pub abs_error: f64,
    pub error: f64,
    pub wape: Option<f64>,
    pub bias: Option<f64>,
    pub rmse: f64,
}

/// Out-of-sample one-step-ahead backtest over the last `test_len` weeks.
pub fn backtest(method: Method, y: &[f64], test_len: usize) -> Backtest {
    let n = y.len();
    let train_end = n - test_len;
    let p = fit(method, y, train_end);
    let f = forecasts(method, p, y);
    let (mut act, mut abs, mut err, mut sq) = (0.0, 0.0, 0.0, 0.0);
    for t in train_end..n {
        let e = f[t] - y[t];
        act += y[t];
        abs += e.abs();
        err += e;
        sq += e * e;
    }
    Backtest {
        method,
        alpha: p.alpha,
        beta: p.beta,
        test_weeks: test_len,
        actual: act,
        abs_error: abs,
        error: err,
        wape: (act > 0.0).then(|| abs / act),
        bias: (act > 0.0).then(|| err / act),
        rmse: (sq / test_len as f64).sqrt(),
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DemandClass {
    Smooth,
    Erratic,
    Intermittent,
    Lumpy,
    NoDemand,
}

/// Syntetos-Boylan classification: ADI (average inter-demand interval) 1.32, CV^2 of sizes 0.49.
pub fn classify(y: &[f64]) -> (DemandClass, f64, f64) {
    let nz: Vec<f64> = y.iter().copied().filter(|v| *v > 0.0).collect();
    if nz.is_empty() {
        return (DemandClass::NoDemand, f64::INFINITY, 0.0);
    }
    let adi = y.len() as f64 / nz.len() as f64;
    let mean = nz.iter().sum::<f64>() / nz.len() as f64;
    let var = nz.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / nz.len() as f64;
    let cv2 = var / (mean * mean);
    let class = match (adi < 1.32, cv2 < 0.49) {
        (true, true) => DemandClass::Smooth,
        (true, false) => DemandClass::Erratic,
        (false, true) => DemandClass::Intermittent,
        (false, false) => DemandClass::Lumpy,
    };
    (class, adi, cv2)
}

pub const MIN_WEEKS_FOR_BACKTEST: usize = 26;

#[derive(Clone, Debug, Serialize)]
pub struct Selected {
    pub method: Method,
    pub forecast_per_week: f64,
    /// one-step forecast error sd (weekly units); used for safety stock. None without a backtest.
    pub error_rmse: Option<f64>,
    pub backtests: Vec<Backtest>,
    pub weeks: usize,
}

/// Run every method, pick by backtest RMSE (or honour `forced`), return the next-week forecast.
pub fn select(y: &[f64], forced: Option<Method>) -> Selected {
    let n = y.len();
    if n < MIN_WEEKS_FOR_BACKTEST {
        // too short to validate anything: robust default, no error estimate
        let m = forced.unwrap_or(Method::Ma13);
        let p = fit(m, y, n);
        return Selected { method: m, forecast_per_week: forecasts(m, p, y)[n], error_rmse: None, backtests: vec![], weeks: n };
    }
    let test_len = (n / 4).clamp(8, 26);
    let bts: Vec<Backtest> = Method::ALL.iter().map(|m| backtest(*m, y, test_len)).collect();
    let chosen = match forced {
        Some(m) => m,
        None => bts.iter().min_by(|a, b| a.rmse.partial_cmp(&b.rmse).unwrap_or(std::cmp::Ordering::Equal)).map(|b| b.method).unwrap_or(Method::Ma13),
    };
    let bt = bts.iter().find(|b| b.method == chosen).cloned();
    let p = bt.as_ref().map(|b| Params { alpha: b.alpha, beta: b.beta }).unwrap_or(Params { alpha: 0.1, beta: 0.05 });
    Selected {
        method: chosen,
        forecast_per_week: forecasts(chosen, p, y)[n].max(0.0),
        error_rmse: bt.as_ref().map(|b| b.rmse),
        backtests: bts,
        weeks: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_demand_is_forecast_exactly() {
        let y = vec![5.0; 40];
        for m in Method::ALL {
            let f = forecasts(m, Params { alpha: 0.2, beta: 0.1 }, &y);
            assert!((f[40] - 5.0).abs() < 0.6, "{:?} -> {}", m, f[40]);
        }
        assert!((forecasts(Method::Ma13, Params { alpha: 0.0, beta: 0.0 }, &y)[40] - 5.0).abs() < 1e-12);
        assert!((forecasts(Method::Ses, Params { alpha: 0.2, beta: 0.0 }, &y)[40] - 5.0).abs() < 1e-12);
    }

    #[test]
    fn ses_matches_hand_calculation() {
        let f = forecasts(Method::Ses, Params { alpha: 0.5, beta: 0.0 }, &[10.0, 20.0, 0.0]);
        assert_eq!(f[1], 10.0);
        assert_eq!(f[2], 15.0);
        assert_eq!(f[3], 7.5);
    }

    #[test]
    fn croston_sba_on_regular_intermittent_demand() {
        // demand of 10 every 4th week -> rate 2.5/week; SBA is biased low by (1 - alpha/2)
        let y: Vec<f64> = (0..60).map(|i| if i % 4 == 3 { 10.0 } else { 0.0 }).collect();
        let a = 0.1;
        let f = forecasts(Method::Sba, Params { alpha: a, beta: 0.0 }, &y)[60];
        assert!((f - 2.5 * (1.0 - a / 2.0)).abs() < 0.05, "{f}");
    }

    #[test]
    fn tsb_decays_after_demand_stops_but_sba_does_not() {
        let mut y: Vec<f64> = (0..30).map(|i| if i % 2 == 0 { 8.0 } else { 0.0 }).collect();
        y.extend(vec![0.0; 40]); // obsolete item
        let tsb = forecasts(Method::Tsb, Params { alpha: 0.1, beta: 0.1 }, &y)[70];
        let sba = forecasts(Method::Sba, Params { alpha: 0.1, beta: 0.1 }, &y)[70];
        assert!(tsb < 0.3, "tsb {tsb}");
        assert!(sba > 1.5, "sba {sba}"); // Croston-type methods keep forecasting the old rate
    }

    #[test]
    fn classification_quadrants() {
        assert_eq!(classify(&vec![10.0; 20]).0, DemandClass::Smooth);
        assert_eq!(classify(&[1.0, 30.0, 2.0, 25.0, 1.0, 40.0, 3.0, 28.0]).0, DemandClass::Erratic);
        let inter: Vec<f64> = (0..40).map(|i| if i % 4 == 0 { 10.0 } else { 0.0 }).collect();
        assert_eq!(classify(&inter).0, DemandClass::Intermittent);
        let lumpy: Vec<f64> = (0..40).map(|i| match i % 8 { 0 => 2.0, 4 => 50.0, _ => 0.0 }).collect();
        assert_eq!(classify(&lumpy).0, DemandClass::Lumpy);
        assert_eq!(classify(&[0.0; 5]).0, DemandClass::NoDemand);
    }

    #[test]
    fn auto_prefers_an_intermittent_method_for_sparse_demand() {
        let y: Vec<f64> = (0..104).map(|i| if i % 5 == 1 { 6.0 + (i % 3) as f64 } else { 0.0 }).collect();
        let s = select(&y, None);
        assert!(s.backtests.len() == 5 && s.error_rmse.is_some());
        let best = s.backtests.iter().map(|b| b.rmse).fold(f64::INFINITY, f64::min);
        assert!((s.error_rmse.unwrap() - best).abs() < 1e-12);
        assert!(s.forecast_per_week > 0.5 && s.forecast_per_week < 3.0, "{}", s.forecast_per_week);
    }

    #[test]
    fn short_history_falls_back_without_backtest() {
        let s = select(&[3.0, 4.0, 2.0, 5.0], None);
        assert_eq!(s.method, Method::Ma13);
        assert!(s.error_rmse.is_none());
        assert!((s.forecast_per_week - 3.5).abs() < 1e-12);
    }
}

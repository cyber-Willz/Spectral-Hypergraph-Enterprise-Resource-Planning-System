//! Pure pay-benchmark maths: ordinary least squares and the design matrix for log-salary.
//!
//! Nothing here is fitted offline. The model is refitted on the company's own employees every
//! time, because the datasets tested disagree about what drives pay (see
//! `docs/DATASET_EVALUATION.md`), so shipping fixed coefficients would present one synthetic
//! dataset's quirks as a market rate. The implementation is validated against scikit-learn on the
//! salary dataset by `dataset_eval`.

use std::collections::BTreeMap;

pub const EDUCATION_ORDER: [&str; 4] = ["high_school", "bachelor", "master", "phd"];

#[derive(Debug, Clone)]
pub struct Ols {
    /// `[intercept, slope_1 .. slope_k]`.
    pub coef: Vec<f64>,
    pub r2: f64,
    pub adj_r2: f64,
    /// Residual standard deviation, with n - k - 1 degrees of freedom.
    pub sigma: f64,
    pub n: usize,
    pub k: usize,
}

impl Ols {
    pub fn predict(&self, row: &[f64]) -> f64 {
        self.coef[0] + self.coef[1..].iter().zip(row).map(|(b, x)| b * x).sum::<f64>()
    }
}

/// Ordinary least squares with an intercept. `rows` hold the predictors only.
/// Columns are centred before the normal equations are solved, and a near-singular system
/// (collinear predictors) is reported rather than returning garbage.
pub fn fit_ols(rows: &[Vec<f64>], y: &[f64]) -> Result<Ols, String> {
    let n = rows.len();
    if n != y.len() {
        return Err("rows and targets differ in length".into());
    }
    let k = rows.first().map_or(0, Vec::len);
    if n <= k + 1 {
        return Err(format!("need more than {} observations for {k} predictors", k + 1));
    }
    let nf = n as f64;
    let ybar = y.iter().sum::<f64>() / nf;
    let xbar: Vec<f64> = (0..k).map(|j| rows.iter().map(|r| r[j]).sum::<f64>() / nf).collect();

    let mut a = vec![vec![0.0; k]; k];
    let mut b = vec![0.0; k];
    for (r, yi) in rows.iter().zip(y) {
        let c: Vec<f64> = (0..k).map(|j| r[j] - xbar[j]).collect();
        for i in 0..k {
            b[i] += c[i] * (yi - ybar);
            for j in 0..k {
                a[i][j] += c[i] * c[j];
            }
        }
    }
    let scale = (0..k).map(|i| a[i][i]).fold(0.0f64, f64::max);
    let slopes = solve(a, b, scale * 1e-10)?;
    let intercept = ybar - slopes.iter().zip(&xbar).map(|(s, x)| s * x).sum::<f64>();
    let mut coef = vec![intercept];
    coef.extend(slopes);

    let model = Ols { coef, r2: 0.0, adj_r2: 0.0, sigma: 0.0, n, k };
    let sse: f64 = rows.iter().zip(y).map(|(r, yi)| (yi - model.predict(r)).powi(2)).sum();
    let sst: f64 = y.iter().map(|v| (v - ybar).powi(2)).sum();
    if sst == 0.0 {
        return Err("salaries do not vary".into());
    }
    let r2 = 1.0 - sse / sst;
    let dof = (n - k - 1) as f64;
    Ok(Ols { r2, adj_r2: 1.0 - (1.0 - r2) * (nf - 1.0) / dof, sigma: (sse / dof).sqrt(), ..model })
}

/// Gaussian elimination with partial pivoting; fails when a pivot falls below `tol`.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>, tol: f64) -> Result<Vec<f64>, String> {
    let k = b.len();
    for col in 0..k {
        let piv = (col..k)
            .max_by(|&i, &j| a[i][col].abs().partial_cmp(&a[j][col].abs()).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(col);
        if a[piv][col].abs() <= tol {
            return Err("predictors are collinear or constant".into());
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..k {
            let f = a[r][col] / a[col][col];
            for c in col..k {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0; k];
    for i in (0..k).rev() {
        let s: f64 = (i + 1..k).map(|j| a[i][j] * x[j]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Ok(x)
}

#[derive(Debug, Clone)]
pub struct PayRow {
    pub salary: f64,
    pub experience_years: f64,
    pub education: String,
    pub city_tier: f64,
    pub remote: bool,
    pub job_title: String,
}

pub struct Design {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<f64>>,
}

fn normalise_title(t: &str) -> String {
    t.trim().to_lowercase()
}

/// Builds predictors for log-salary: experience, city tier, remote, education dummies (baseline =
/// lowest level present) and job-title dummies for titles held by at least `min_title_count`
/// people. Rare titles share the baseline. Constant columns are dropped.
pub fn build_design(rows: &[PayRow], min_title_count: usize) -> Design {
    let mut columns = vec!["years_experience".to_string(), "city_tier".into(), "remote_work".into()];
    let mut cols: Vec<Vec<f64>> = vec![
        rows.iter().map(|r| r.experience_years).collect(),
        rows.iter().map(|r| r.city_tier).collect(),
        rows.iter().map(|r| r.remote as u8 as f64).collect(),
    ];

    let present: Vec<&str> = EDUCATION_ORDER
        .iter()
        .copied()
        .filter(|l| rows.iter().any(|r| r.education == *l))
        .collect();
    for level in present.iter().skip(1) {
        columns.push(format!("education: {level}"));
        cols.push(rows.iter().map(|r| (r.education == *level) as u8 as f64).collect());
    }

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        *counts.entry(normalise_title(&r.job_title)).or_default() += 1;
    }
    let mut common: Vec<(String, usize)> = counts.iter().filter(|(_, c)| **c >= min_title_count).map(|(t, c)| (t.clone(), *c)).collect();
    let has_rare = counts.values().any(|c| *c < min_title_count);
    common.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !has_rare && !common.is_empty() {
        common.remove(0); // the largest group becomes the baseline
    }
    for (title, _) in common {
        columns.push(format!("title: {title}"));
        cols.push(rows.iter().map(|r| (normalise_title(&r.job_title) == title) as u8 as f64).collect());
    }

    let keep: Vec<usize> = (0..cols.len())
        .filter(|&j| cols[j].iter().any(|v| (*v - cols[j][0]).abs() > 1e-12))
        .collect();
    Design {
        columns: keep.iter().map(|&j| columns[j].clone()).collect(),
        rows: (0..rows.len()).map(|i| keep.iter().map(|&j| cols[j][i]).collect()).collect(),
    }
}

/// Percent change in salary implied by a log-salary coefficient.
pub fn effect_pct(coef: f64) -> f64 {
    (coef.exp() - 1.0) * 100.0
}

pub fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 })
}

pub fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let n = a.len();
    if n < 3 || n != b.len() {
        return None;
    }
    let (ma, mb) = (a.iter().sum::<f64>() / n as f64, b.iter().sum::<f64>() / n as f64);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb): (f64, f64) = (a.iter().map(|x| (x - ma).powi(2)).sum(), b.iter().map(|y| (y - mb).powi(2)).sum());
    (va > 0.0 && vb > 0.0).then(|| cov / (va * vb).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_exact_linear_relationship() {
        // y = 2 + 3*x1 - 0.5*x2
        let rows: Vec<Vec<f64>> = (0..30).map(|i| vec![i as f64, ((i * 7) % 11) as f64]).collect();
        let y: Vec<f64> = rows.iter().map(|r| 2.0 + 3.0 * r[0] - 0.5 * r[1]).collect();
        let m = fit_ols(&rows, &y).unwrap();
        assert!((m.coef[0] - 2.0).abs() < 1e-9 && (m.coef[1] - 3.0).abs() < 1e-9 && (m.coef[2] + 0.5).abs() < 1e-9);
        assert!((m.r2 - 1.0).abs() < 1e-12 && m.sigma < 1e-6);
    }

    #[test]
    fn matches_known_noisy_fit() {
        // Hand-checkable: y on x with points (1,2) (2,3) (3,5) (4,4) (5,6): slope 0.9, intercept 1.3.
        let rows: Vec<Vec<f64>> = (1..=5).map(|i| vec![i as f64]).collect();
        let m = fit_ols(&rows, &[2.0, 3.0, 5.0, 4.0, 6.0]).unwrap();
        assert!((m.coef[1] - 0.9).abs() < 1e-12 && (m.coef[0] - 1.3).abs() < 1e-12);
        assert!((m.r2 - 0.81).abs() < 1e-12);
    }

    #[test]
    fn collinear_predictors_are_rejected() {
        let rows: Vec<Vec<f64>> = (0..20).map(|i| vec![i as f64, 2.0 * i as f64]).collect();
        let y: Vec<f64> = (0..20).map(|i| i as f64 + (i % 3) as f64).collect();
        assert!(fit_ols(&rows, &y).is_err());
    }

    #[test]
    fn too_few_observations_are_rejected() {
        let rows = vec![vec![1.0, 2.0], vec![2.0, 1.0], vec![3.0, 5.0]];
        assert!(fit_ols(&rows, &[1.0, 2.0, 3.0]).is_err());
    }

    fn row(sal: f64, exp: f64, edu: &str, tier: f64, remote: bool, title: &str) -> PayRow {
        PayRow { salary: sal, experience_years: exp, education: edu.into(), city_tier: tier, remote, job_title: title.into() }
    }

    #[test]
    fn design_uses_lowest_present_education_as_baseline_and_drops_constants() {
        let rows = vec![
            row(1.0, 1.0, "bachelor", 1.0, false, "Dev"),
            row(1.0, 2.0, "master", 1.0, false, "Dev"),
            row(1.0, 3.0, "phd", 1.0, false, "QA"),
        ];
        let d = build_design(&rows, 5);
        // city_tier and remote_work are constant => dropped; bachelor is the baseline.
        assert_eq!(d.columns, vec!["years_experience", "education: master", "education: phd"]);
        assert_eq!(d.rows[1], vec![2.0, 1.0, 0.0]);
    }

    #[test]
    fn design_title_dummies_avoid_the_dummy_trap() {
        let mut rows = vec![];
        for i in 0..6 {
            rows.push(row(1.0, i as f64, "bachelor", 1.0 + (i % 2) as f64, i % 2 == 0, "Dev"));
            rows.push(row(1.0, i as f64, "bachelor", 1.0 + (i % 3) as f64, i % 3 == 0, "QA"));
        }
        let d = build_design(&rows, 5);
        // two common titles and no rare ones => only one dummy (the other is the baseline)
        assert_eq!(d.columns.iter().filter(|c| c.starts_with("title:")).count(), 1);
    }

    #[test]
    fn helpers() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert!((effect_pct(0.1) - 10.517).abs() < 0.01);
        assert!((pearson(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]).unwrap() - 1.0).abs() < 1e-12);
        assert!(pearson(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]).is_none());
    }
}

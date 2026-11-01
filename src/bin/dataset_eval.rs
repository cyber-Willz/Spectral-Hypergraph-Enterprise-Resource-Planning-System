//! Evaluates the risk models shipped in this crate against the reference datasets.
//!
//! usage: dataset_eval <data_dir> [<data_dir> ...]
//! where each <data_dir> holds one sub-directory per dataset (as unzipped); sections whose files
//! are not found are skipped. Expected layout, e.g.
//!   Enterprise_Financial_Network_Dataset/enterprise_nodes_2934.csv
//!   Entrepreneurship_Risk_Assessment_Dataset/entrepreneurship_risk_assessment_dataset.csv
//!   Corporate_Financial_Risk_Monitoring_Dataset/enterprise_financial_dataset.csv
//!   Financial_Statement_Anomaly_Dataset/Financial Statement Anomaly Dataset.csv

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use enterprise_suite::erp::{
    hr::pay_model::{build_design, effect_pct, fit_ols, PayRow},
    risk::model::{counterparty_score, health_band, health_score, CounterpartyInputs, HealthInputs},
};

struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// Loads `rel` from the first directory that has it; `None` if no directory does.
fn load(dirs: &[PathBuf], rel: &str) -> anyhow::Result<Option<Table>> {
    for d in dirs {
        let p = d.join(rel);
        if p.exists() {
            return Ok(Some(Table::load(&p)?));
        }
    }
    println!("  (skipped: {rel} not found)");
    Ok(None)
}

impl Table {
    fn load(path: &Path) -> anyhow::Result<Self> {
        let mut rdr = csv::Reader::from_path(path)?;
        let headers = rdr.headers()?.iter().map(|h| h.trim().to_owned()).collect();
        let rows = rdr
            .records()
            .map(|r| r.map(|r| r.iter().map(str::to_owned).collect()))
            .collect::<Result<_, _>>()?;
        Ok(Self { headers, rows })
    }
    fn idx(&self, name: &str) -> anyhow::Result<usize> {
        self.headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| anyhow::anyhow!("column {name} not found"))
    }
    fn num(&self, name: &str) -> anyhow::Result<Vec<f64>> {
        let i = self.idx(name)?;
        self.rows
            .iter()
            .map(|r| r[i].trim().parse::<f64>().map_err(|e| anyhow::anyhow!("{name}: {e}")))
            .collect()
    }
    fn text(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let i = self.idx(name)?;
        Ok(self.rows.iter().map(|r| r[i].clone()).collect())
    }
}

/// Mann-Whitney AUC with average ranks for ties. `labels` are 0/1.
fn auc(scores: &[f64], labels: &[f64]) -> f64 {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&a, &b| scores[a].partial_cmp(&scores[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0; scores.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && scores[idx[j + 1]] == scores[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j {
            ranks[idx[k]] = avg;
        }
        i = j + 1;
    }
    let pos = labels.iter().filter(|&&l| l > 0.5).count() as f64;
    let neg = labels.len() as f64 - pos;
    let sum: f64 = ranks.iter().zip(labels).filter(|(_, &l)| l > 0.5).map(|(r, _)| r).sum();
    (sum - pos * (pos + 1.0) / 2.0) / (pos * neg)
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb): (f64, f64) = (a.iter().map(|x| (x - ma).powi(2)).sum(), b.iter().map(|y| (y - mb).powi(2)).sum());
    cov / (va * vb).sqrt()
}

/// Best accuracy achievable by a one-feature threshold rule (a decision stump) on any column.
fn best_stump(features: &BTreeMap<String, Vec<f64>>, labels: &[String]) -> (String, f64) {
    let n = labels.len() as f64;
    let mut best = (String::new(), 0.0);
    for (name, col) in features {
        let mut sorted = col.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for q in 1..50 {
            let thr = sorted[sorted.len() * q / 50];
            let mut sides: [BTreeMap<&str, usize>; 2] = [BTreeMap::new(), BTreeMap::new()];
            for (v, l) in col.iter().zip(labels) {
                *sides[(*v > thr) as usize].entry(l.as_str()).or_default() += 1;
            }
            let correct: usize = sides.iter().map(|m| m.values().copied().max().unwrap_or(0)).sum();
            let acc = correct as f64 / n;
            if acc > best.1 {
                best = (name.clone(), acc);
            }
        }
    }
    best
}

fn majority(labels: &[String]) -> (String, f64) {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for l in labels {
        *m.entry(l).or_default() += 1;
    }
    let (k, v) = m.into_iter().max_by_key(|(_, v)| *v).unwrap_or(("", 0));
    (k.to_owned(), v as f64 / labels.len() as f64)
}

fn counterparty(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 1. Counterparty credit score  (Enterprise_Financial_Network_Dataset, nodes) ===");
    let Some(t) = load(dirs, "Enterprise_Financial_Network_Dataset/enterprise_nodes_2934.csv")? else { return Ok(()) };
    let (credit, debt, liq, bankrupt) = (t.num("credit_score")?, t.num("debt_ratio")?, t.num("liquidity")?, t.num("bankrupt")?);
    let scores: Vec<_> = (0..credit.len())
        .map(|i| counterparty_score(CounterpartyInputs { credit_score: credit[i], debt_ratio: debt[i], liquidity: liq[i] }))
        .collect();
    let p: Vec<f64> = scores.iter().map(|s| s.probability).collect();
    println!("rows: {}   bankrupt base rate: {:.1}%", p.len(), 100.0 * bankrupt.iter().sum::<f64>() / bankrupt.len() as f64);
    println!("AUC (in-sample; coefficients were fitted on this file, 5-fold CV AUC is 0.719): {:.3}", auc(&p, &bankrupt));
    println!("rows flagged extrapolated: {}", scores.iter().filter(|s| s.extrapolated).count());
    let mut bands: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for (s, b) in scores.iter().zip(&bankrupt) {
        let e = bands.entry(s.band).or_default();
        e.0 += 1;
        e.1 += b;
    }
    println!("{:<8}{:>8}{:>18}", "band", "n", "observed bankrupt");
    for band in ["low", "medium", "high"] {
        let (n, b) = bands.get(band).copied().unwrap_or_default();
        println!("{:<8}{:>8}{:>17.1}%", band, n, if n > 0 { 100.0 * b / n as f64 } else { 0.0 });
    }
    Ok(())
}

fn health(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 2. Financial health score  (Entrepreneurship_Risk_Assessment_Dataset) ===");
    let Some(t) = load(dirs, "Entrepreneurship_Risk_Assessment_Dataset/entrepreneurship_risk_assessment_dataset.csv")? else { return Ok(()) };
    let (d, c, m, r, target) = (
        t.num("Debt_Ratio_Percent")?,
        t.num("Current_Ratio")?,
        t.num("Profit_Margin_Percent")?,
        t.num("Runway_Months")?,
        t.num("Financial_Risk_Score")?,
    );
    let mut pred = vec![];
    let mut clamped_rows = 0;
    for i in 0..d.len() {
        let s = health_score(HealthInputs { debt_ratio_pct: d[i], current_ratio: c[i], profit_margin_pct: m[i], runway_months: r[i] });
        clamped_rows += (!s.clamped.is_empty()) as usize;
        pred.push(s.score);
    }
    let n = pred.len() as f64;
    let mae = pred.iter().zip(&target).map(|(a, b)| (a - b).abs()).sum::<f64>() / n;
    let rmse = (pred.iter().zip(&target).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / n).sqrt();
    let within5 = pred.iter().zip(&target).filter(|(a, b)| (*a - *b).abs() <= 5.0).count() as f64 / n;
    let band_match = pred.iter().zip(&target).filter(|(a, b)| health_band(**a) == health_band(**b)).count() as f64 / n;
    println!("rows: {}   rows with a clamped input: {}", d.len(), clamped_rows);
    println!("Pearson r vs dataset Financial_Risk_Score: {:.3}   (R^2 {:.3})", pearson(&pred, &target), pearson(&pred, &target).powi(2));
    println!("MAE {:.2} pts   RMSE {:.2} pts   within +/-5 pts: {:.1}%   same band: {:.1}%", mae, rmse, 100.0 * within5, 100.0 * band_match);

    let level = t.text("Risk_Level")?;
    let (maj, share) = majority(&level);
    println!("note: dataset Risk_Level is {:.1}% '{}', so it cannot discriminate; this score is checked against Financial_Risk_Score instead", 100.0 * share, maj);
    Ok(())
}

fn label_noise(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 3. Corporate_Financial_Risk_Monitoring_Dataset ===");
    let Some(t) = load(dirs, "Corporate_Financial_Risk_Monitoring_Dataset/enterprise_financial_dataset.csv")? else { return Ok(()) };
    let (rev, opc, net, cf) = (t.num("Revenue")?, t.num("Operating_Cost")?, t.num("Net_Profit")?, t.num("Cash_Flow")?);
    let n = rev.len();
    let cnt = |f: &dyn Fn(usize) -> bool| (0..n).filter(|&i| f(i)).count();
    println!("rows: {n}");
    println!("operating cost > revenue            : {:>5} ({:.1}%)", cnt(&|i| opc[i] > rev[i]), 100.0 * cnt(&|i| opc[i] > rev[i]) as f64 / n as f64);
    println!("net profit > revenue                : {:>5} ({:.1}%)", cnt(&|i| net[i] > rev[i]), 100.0 * cnt(&|i| net[i] > rev[i]) as f64 / n as f64);
    println!("net profit > revenue - operating cost: {:>5} ({:.1}%)", cnt(&|i| net[i] > rev[i] - opc[i] + 1.0), 100.0 * cnt(&|i| net[i] > rev[i] - opc[i] + 1.0) as f64 / n as f64);
    println!("cash flow > revenue                 : {:>5}", cnt(&|i| cf[i] > rev[i]));

    let class = t.text("Risk_Class")?;
    let tag = t.text("Risk_Tag")?;
    let mut feats = BTreeMap::new();
    for name in t.headers.iter().filter(|h| !matches!(h.as_str(), "Risk_Tag" | "Risk_Class")) {
        feats.insert(name.clone(), t.num(name)?);
    }
    let (maj, share) = majority(&class);
    let (f, acc) = best_stump(&feats, &class);
    println!("Risk_Class  majority baseline {:.1}% (class {maj});  best one-feature rule {:.1}% using {f}", 100.0 * share, 100.0 * acc);
    let (maj, share) = majority(&tag);
    let (f, acc) = best_stump(&feats, &tag);
    println!("Risk_Tag    majority baseline {:.1}% ({maj});  best one-feature rule {:.1}% using {f}", 100.0 * share, 100.0 * acc);

    // Does our own score order the dataset's classes? (Runway is not in this file; held at 48.)
    let (debt, cr) = (t.num("Debt_Ratio")?, t.num("Liquidity_Ratio")?);
    let mut by: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for i in 0..n {
        let s = health_score(HealthInputs { debt_ratio_pct: debt[i] * 100.0, current_ratio: cr[i], profit_margin_pct: 100.0 * net[i] / rev[i], runway_months: 48.0 });
        let e = by.entry(class[i].clone()).or_default();
        e.0 += s.score;
        e.1 += 1;
    }
    print!("mean health score by Risk_Class (runway fixed at 48):");
    for (k, (s, c)) in &by {
        print!("  class {k}: {:.1}", s / *c as f64);
    }
    println!();
    Ok(())
}

fn anomaly(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 4. Financial_Statement_Anomaly_Dataset ===");
    let Some(t) = load(dirs, "Financial_Statement_Anomaly_Dataset/Financial Statement Anomaly Dataset.csv")? else { return Ok(()) };
    let status = t.text("Financial_Status")?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &status {
        *counts.entry(s).or_default() += 1;
    }
    let mut feats = BTreeMap::new();
    for name in t.headers.iter().filter(|h| h.as_str() != "Financial_Status") {
        feats.insert(name.clone(), t.num(name)?);
    }
    let max_abs = feats.values().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
    let (maj, share) = majority(&status);
    let (f, acc) = best_stump(&feats, &status);
    println!("rows: {}   classes: {:?}", status.len(), counts);
    println!("all feature values lie within +/-{max_abs:.2}: the file is standardised, so ratios cannot be mapped to ledger amounts");
    println!("majority baseline {:.1}% ({maj});  best one-feature rule {:.1}% using {f}", 100.0 * share, 100.0 * acc);
    Ok(())
}

fn onehot(vals: &[f64]) -> Vec<Vec<f64>> {
    let mut levels: Vec<f64> = vals.to_vec();
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    levels.dedup();
    vals.iter().map(|v| levels.iter().skip(1).map(|l| (v == l) as u8 as f64).collect()).collect()
}

fn hstack(parts: Vec<Vec<Vec<f64>>>) -> Vec<Vec<f64>> {
    let n = parts[0].len();
    (0..n).map(|i| parts.iter().flat_map(|p| p[i].clone()).collect()).collect()
}

/// 5-fold CV (fold = row index mod 5): returns (R^2 on log-salary, MAPE on salary).
fn cv_log_fit(x: &[Vec<f64>], y_log: &[f64]) -> anyhow::Result<(f64, f64)> {
    let n = x.len();
    let mut pred = vec![0.0; n];
    for fold in 0..5 {
        let (tx, ty): (Vec<_>, Vec<_>) = (0..n).filter(|i| i % 5 != fold).map(|i| (x[i].clone(), y_log[i])).unzip();
        let m = fit_ols(&tx, &ty).map_err(|e| anyhow::anyhow!(e))?;
        for i in (0..n).filter(|i| i % 5 == fold) {
            pred[i] = m.predict(&x[i]);
        }
    }
    let mean = y_log.iter().sum::<f64>() / n as f64;
    let sse: f64 = y_log.iter().zip(&pred).map(|(a, b)| (a - b).powi(2)).sum();
    let sst: f64 = y_log.iter().map(|a| (a - mean).powi(2)).sum();
    let mape = y_log.iter().zip(&pred).map(|(a, b)| ((b.exp() - a.exp()) / a.exp()).abs()).sum::<f64>() / n as f64;
    Ok((1.0 - sse / sst, mape))
}

fn pay(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 5. Pay benchmark model  (Employee_Compensation__Salary_Prediction_Dataset) ===");
    let Some(t) = load(dirs, "Employee_Compensation__Salary_Prediction_Dataset/employee_salary_regression.csv")? else { return Ok(()) };
    let (exp, tier, remote, sal) = (t.num("years_experience")?, t.num("city_tier")?, t.num("remote_work")?, t.num("annual_salary_usd")?);
    let (edu, role) = (t.text("education_level")?, t.text("job_role")?);
    let rows: Vec<PayRow> = (0..sal.len())
        .map(|i| PayRow {
            salary: sal[i],
            experience_years: exp[i],
            education: edu[i].to_lowercase().replace(' ', "_"),
            city_tier: tier[i],
            remote: remote[i] > 0.5,
            job_title: role[i].clone(),
        })
        .collect();
    let d = build_design(&rows, 5);
    let y: Vec<f64> = sal.iter().map(|v| v.ln()).collect();
    let m = fit_ols(&d.rows, &y).map_err(|e| anyhow::anyhow!(e))?;
    println!("rows: {}   terms: {}   (age, skills and performance are not inputs)", m.n, m.k);
    println!("in-sample R^2 {:.4}  adj R^2 {:.4}  residual SD {:.4} (typical error {:.1}%)", m.r2, m.adj_r2, m.sigma, (m.sigma.exp() - 1.0) * 100.0);
    let (cv_r2, mape) = cv_log_fit(&d.rows, &y)?;
    println!("5-fold CV: R^2 {cv_r2:.4}   MAPE {:.2}%", mape * 100.0);
    println!("scikit-learn reference for the same terms: see docs (R^2 / MAPE must match)");
    print!("effects (% pay):");
    for (c, b) in d.columns.iter().zip(&m.coef[1..]) {
        print!("  {c} {:+.1}", effect_pct(*b));
    }
    println!();
    // minimum-sample behaviour: refit on the first n rows and test on the rest
    for n in [30usize, 50, 100, 200] {
        let tr = &d.rows[..n];
        if let Ok(f) = fit_ols(tr, &y[..n]) {
            let (te_x, te_y) = (&d.rows[n..], &y[n..]);
            let mean = te_y.iter().sum::<f64>() / te_y.len() as f64;
            let sse: f64 = te_x.iter().zip(te_y).map(|(r, v)| (v - f.predict(r)).powi(2)).sum();
            let sst: f64 = te_y.iter().map(|v| (v - mean).powi(2)).sum();
            println!("  fit on first {n:>3} rows -> held-out R^2 {:.3}", 1.0 - sse / sst);
        }
    }

    println!("\n--- same code on the other two pay datasets (is the structure general?) ---");
    if let Some(e) = load(dirs, "Employee_Performance_Metrics/Employee Performance Data.csv")? {
        let sal: Vec<f64> = e.num("Monthly_Salary_USD")?.iter().map(|v| v.ln()).collect();
        let base = hstack(vec![
            e.num("Years_of_Experience")?.iter().map(|v| vec![*v]).collect(),
            onehot(&e.num("Education_Level")?),
            onehot(&e.num("Department")?),
            onehot(&e.num("Job_Role")?),
        ]);
        let m0 = fit_ols(&base, &sal).map_err(|x| anyhow::anyhow!(x))?;
        let with_perf = hstack(vec![base.clone(), e.num("Performance_Score")?.iter().map(|v| vec![*v]).collect()]);
        let m1 = fit_ols(&with_perf, &sal).map_err(|x| anyhow::anyhow!(x))?;
        println!("Employee_Performance_Metrics: log-salary R^2 {:.3} from experience+education+department+role; {:.3} once Performance_Score is added", m0.r2, m1.r2);
    }
    println!("Employee_performance_classification (130k xlsx): R^2 -0.001 on the same inputs (measured in Python; xlsx is not read here)");
    Ok(())
}

fn performance(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 6. Performance datasets ===");
    if let Some(e) = load(dirs, "Employee_Performance_Metrics/Employee Performance Data.csv")? {
        let (score, rating) = (e.num("Performance_Score")?, e.num("Performance_Rating")?);
        // LabelEncoder is alphabetical: Average=0, Excellent=1, Good=2, Poor=3.
        let band = |s: f64| if s >= 85.0 { 1.0 } else if s >= 70.0 { 2.0 } else if s >= 55.0 { 0.0 } else { 3.0 };
        let same = score.iter().zip(&rating).filter(|(s, r)| band(**s) == **r).count();
        println!("Employee_Performance_Metrics: rating == banded score in {}/{} rows ({:.1}%): the rating adds no information beyond the score", same, score.len(), 100.0 * same as f64 / score.len() as f64);
        let share = rating.iter().filter(|r| **r == 1.0).count() as f64 / rating.len() as f64;
        println!("  {:.1}% of employees are rated Excellent; Name/Gender/Department/Role are label-encoded integers with no decode map", 100.0 * share);
    }
    if let Some(h) = load(dirs, "Enterprise_HR_Performance_Dataset/employee_performance_data_categorical.csv")? {
        let class = h.text("performance_class")?;
        let (maj, share) = majority(&class);
        let score = h.num("performance_score")?;
        let (lo, hi) = score.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
        println!("Enterprise_HR: performance_class is '{maj}' for {:.0}% of {} rows although performance_score spans {lo:.1}-{hi:.1}", 100.0 * share, class.len());
        let ids = h.text("employee_id")?;
        let dept = h.text("department")?;
        let mut seen: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
        for (i, d) in ids.iter().zip(&dept) {
            seen.entry(i).or_default().insert(d);
        }
        let varying = seen.values().filter(|s| s.len() > 1).count();
        println!("  {varying} of {} employees change department between months (department/role/education/salary band all drift)", seen.len());
        let feats = ["years_at_company", "monthly_hours_worked", "attendance_rate", "tasks_completed", "manager_rating", "promotions_last_3years"];
        let cols: Vec<Vec<f64>> = feats.iter().map(|f| h.num(f)).collect::<Result<_, _>>()?;
        let x: Vec<Vec<f64>> = (0..score.len()).map(|i| cols.iter().map(|c| c[i]).collect()).collect();
        let m = fit_ols(&x, &score).map_err(|e| anyhow::anyhow!(e))?;
        println!("  linear fit of performance_score on 6 numeric fields: in-sample R^2 {:.3} (an upper bound; grouped CV R^2 is below 0)", m.r2);
    }
    println!("Employee_performance_classification (130k xlsx): 3 balanced classes, accuracy 33% at best (measured in Python)");
    Ok(())
}

fn accounting(dirs: &[PathBuf]) -> anyhow::Result<()> {
    println!("\n=== 7. Accounting_Data_for_Financial_Management ===");
    let Some(t) = load(dirs, "Accounting_Data_for_Financial_Management/accounting_data.csv")? else { return Ok(()) };
    let (rev, exp, ni, gp) = (t.num("Revenue")?, t.num("Expenditure")?, t.num("Net Income")?, t.num("Gross Profit")?);
    let n = rev.len();
    let pct = |c: usize| 100.0 * c as f64 / n as f64;
    let c1 = (0..n).filter(|&i| ni[i] == rev[i] - exp[i]).count();
    let c2 = (0..n).filter(|&i| gp[i] == rev[i] - exp[i]).count();
    let c3 = (0..n).filter(|&i| ni[i] > rev[i]).count();
    let c4 = (0..n).filter(|&i| gp[i] > rev[i]).count();
    println!("rows: {n}");
    println!("net income == revenue - expenditure : {c1} ({:.1}%)", pct(c1));
    println!("gross profit == revenue - expenditure: {c2} ({:.1}%)", pct(c2));
    println!("net income > revenue                 : {c3} ({:.1}%)", pct(c3));
    println!("gross profit > revenue               : {c4} ({:.1}%)", pct(c4));
    let dates = t.text("Date")?;
    println!("dates run {} to {} (one row per day)", dates.iter().min().cloned().unwrap_or_default(), dates.iter().max().cloned().unwrap_or_default());
    let outcome = t.text("Transaction Outcome")?;
    let mut feats = BTreeMap::new();
    for name in ["Transaction Amount", "Cash Flow", "Net Income", "Revenue", "Expenditure", "Profit Margin", "Debt-to-Equity Ratio", "Operating Expenses", "Gross Profit", "Transaction Volume", "Processing Time (seconds)", "Accuracy Score"] {
        feats.insert(name.to_string(), t.num(name)?);
    }
    let (maj, share) = majority(&outcome);
    let (f, acc) = best_stump(&feats, &outcome);
    println!("Transaction Outcome: majority baseline {:.1}% ({maj}); best one-feature rule {:.1}% using {f}", 100.0 * share, 100.0 * acc);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let dirs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        anyhow::bail!("usage: dataset_eval <data_dir> [<data_dir> ...]");
    }
    counterparty(&dirs)?;
    health(&dirs)?;
    label_noise(&dirs)?;
    anomaly(&dirs)?;
    pay(&dirs)?;
    performance(&dirs)?;
    accounting(&dirs)?;
    Ok(())
}

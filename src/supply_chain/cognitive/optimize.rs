//! Decision optimisation (pure maths, unit-tested): inventory policy from forecast + uncertainty,
//! risk-aware supplier allocation with concentration cap, and warehouse balancing.
use crate::supply_chain::stats::{norm_inv, safety_stock};

use super::analytics::phi;

#[derive(Clone, Debug)]
pub struct InvIn {
    pub mu_week: f64,
    pub sd_week: f64,
    pub lead_days: f64,
    pub lead_sd_days: f64,
    pub review_days: f64,
    pub service_level: f64,
    pub z_mult: f64,
    pub available: f64,
    pub on_order: f64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct InvOut {
    pub safety_stock: f64,
    pub reorder_point: f64,
    pub order_up_to: f64,
    pub position: f64,
    pub order_qty: f64,
    pub stockout_prob_in_lead_time: f64,
    pub days_to_reorder: Option<f64>,
    pub cover_days: Option<f64>,
}

pub fn inventory_policy(i: &InvIn) -> InvOut {
    let mu_d = i.mu_week / 7.0;
    let sd_d = i.sd_week / 7f64.sqrt();
    let z = norm_inv(i.service_level) * i.z_mult;
    let ss = safety_stock(z, mu_d, sd_d, i.lead_days, i.lead_sd_days, i.review_days).max(0.0);
    let rop = mu_d * (i.lead_days + i.review_days) + ss;
    let s_level = rop + mu_d * i.review_days;
    let position = i.available + i.on_order;
    let order_qty = if position <= rop { (s_level - position).max(0.0) } else { 0.0 };
    let mean_l = mu_d * i.lead_days;
    let sd_l = (i.lead_days * sd_d * sd_d + mu_d * mu_d * i.lead_sd_days * i.lead_sd_days).sqrt();
    let p = if sd_l > 1e-9 { 1.0 - phi((position - mean_l) / sd_l) } else if position < mean_l { 1.0 } else { 0.0 };
    InvOut {
        safety_stock: ss,
        reorder_point: rop,
        order_up_to: s_level,
        position,
        order_qty,
        stockout_prob_in_lead_time: p.clamp(0.0, 1.0),
        days_to_reorder: (mu_d > 1e-9).then(|| ((position - rop) / mu_d).max(0.0)),
        cover_days: (mu_d > 1e-9).then(|| i.available / mu_d),
    }
}

#[derive(Clone, Debug)]
pub struct Cand {
    pub supplier: usize,
    pub unit_cost: f64,
    pub lead_days: f64,
    pub risk: f64,
}

/// Fill `qty` from the lowest risk-adjusted landed cost first, no supplier above `max_share` when
/// there are at least two candidates. Cost = unit_cost * (1 + penalty * risk) + holding_per_day * lead.
pub fn allocate(qty: i64, cands: &[Cand], risk_penalty: f64, max_share: f64, holding_per_day: f64) -> Vec<(usize, i64)> {
    if qty <= 0 || cands.is_empty() {
        return vec![];
    }
    let eff = |c: &Cand| c.unit_cost * (1.0 + risk_penalty * c.risk) + holding_per_day * c.lead_days;
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|a, b| eff(&cands[*a]).partial_cmp(&eff(&cands[*b])).unwrap_or(std::cmp::Ordering::Equal));
    let cap = if cands.len() >= 2 { ((max_share.clamp(0.0, 1.0)) * qty as f64).ceil() as i64 } else { qty };
    let mut left = qty;
    let mut out: Vec<(usize, i64)> = Vec::new();
    for &i in &order {
        let take = left.min(cap.max(1));
        if take > 0 {
            out.push((i, take));
            left -= take;
        }
        if left == 0 {
            break;
        }
    }
    if left > 0 {
        out[0].1 += left; // caps too tight for the candidate count: remainder to the best source
    }
    out
}

pub fn hhi(parts: &[i64]) -> f64 {
    let t: f64 = parts.iter().sum::<i64>() as f64;
    if t <= 0.0 { 0.0 } else { parts.iter().map(|p| (*p as f64 / t).powi(2)).sum() }
}

#[derive(Clone, Debug)]
pub struct WhPos {
    pub wh: usize,
    pub available: f64,
    pub mu_day: f64,
}

/// Move stock from warehouses holding more than `hi` days of cover to those below `lo` days
/// (only the surplus above `target` / deficit below `target` is moved).
pub fn balance(pos: &[WhPos], lo: f64, hi: f64, target: f64) -> Vec<(usize, usize, i64)> {
    let cover = |p: &WhPos| if p.mu_day > 1e-9 { p.available / p.mu_day } else { f64::INFINITY };
    // a warehouse holding stock it never sells (no demand) is a donor of everything it has
    let mut donors: Vec<(usize, f64)> = pos.iter().filter(|p| cover(p) > hi && p.available > 0.0).map(|p| (p.wh, p.available - target * p.mu_day.max(0.0))).collect();
    let mut recv: Vec<(usize, f64)> = pos.iter().filter(|p| p.mu_day > 1e-9 && cover(p) < lo).map(|p| (p.wh, target * p.mu_day - p.available)).collect();
    donors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    recv.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    for r in recv.iter_mut() {
        for d in donors.iter_mut() {
            let q = r.1.min(d.1).floor();
            if q >= 1.0 {
                out.push((d.0, r.0, q as i64));
                r.1 -= q;
                d.1 -= q;
            }
            if r.1 < 1.0 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_orders_up_to_level_when_position_below_rop() {
        let i = InvIn { mu_week: 70.0, sd_week: 20.0, lead_days: 14.0, lead_sd_days: 2.0, review_days: 7.0, service_level: 0.95, z_mult: 1.0, available: 50.0, on_order: 0.0 };
        let o = inventory_policy(&i);
        assert!(o.safety_stock > 0.0 && o.reorder_point > 10.0 * 21.0);
        assert!(o.order_qty > 0.0 && (o.position + o.order_qty - o.order_up_to).abs() < 1e-9);
        assert!(o.stockout_prob_in_lead_time > 0.5);
        let full = inventory_policy(&InvIn { available: 1000.0, ..i.clone() });
        assert_eq!(full.order_qty, 0.0);
        assert!(full.stockout_prob_in_lead_time < 0.05);
        // higher z multiplier -> more safety stock
        assert!(inventory_policy(&InvIn { z_mult: 1.3, ..i }).safety_stock > o.safety_stock);
    }

    #[test]
    fn allocation_respects_cap_and_prefers_low_risk_adjusted_cost() {
        let c = vec![
            Cand { supplier: 0, unit_cost: 10.0, lead_days: 7.0, risk: 0.6 }, // cheap but risky
            Cand { supplier: 1, unit_cost: 10.6, lead_days: 7.0, risk: 0.05 },
            Cand { supplier: 2, unit_cost: 12.0, lead_days: 7.0, risk: 0.05 },
        ];
        let a = allocate(100, &c, 1.0, 0.7, 0.0);
        assert_eq!(a.iter().map(|x| x.1).sum::<i64>(), 100);
        assert_eq!(a[0].0, 1, "risk-adjusted cheapest first");
        assert!(a.iter().all(|x| x.1 <= 70));
        assert!(hhi(&a.iter().map(|x| x.1).collect::<Vec<_>>()) < 1.0);
        let solo = allocate(40, &c[..1], 1.0, 0.7, 0.0);
        assert_eq!(solo, vec![(0, 40)]);
    }

    #[test]
    fn balancing_moves_surplus_to_deficit_without_overshoot() {
        let p = vec![WhPos { wh: 0, available: 1000.0, mu_day: 10.0 }, WhPos { wh: 1, available: 20.0, mu_day: 10.0 }];
        let t = balance(&p, 14.0, 60.0, 30.0);
        assert_eq!(t, vec![(0, 1, 280)]);
        // stock sitting where nothing is sold feeds the warehouse that sells it
        let z = vec![WhPos { wh: 0, available: 500.0, mu_day: 0.0 }, WhPos { wh: 1, available: 0.0, mu_day: 10.0 }];
        assert_eq!(balance(&z, 14.0, 90.0, 30.0), vec![(0, 1, 300)]);
    }
}

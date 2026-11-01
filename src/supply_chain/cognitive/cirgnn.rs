//! CIR-GNN: Connectome-Informed Recurrent Graph Neural Network.
//!
//! Every supply-network node is a "region" of a `hcrnn::Connectome` (flow strength = tract weight,
//! lead time in weeks = fibre length, so conduction delay = transit lead time). Each node has an
//! excitatory unit E (propagates demand / disruption) and an inhibitory unit I (buffering).
//! Wiring, Dale's-law signs, delays and the Arnoldi (krylov_ds) spectral calibration come from
//! `hcrnn`; the recurrent core below adds what a supply chain needs and the brain task did not:
//! node-wise input injection (type-shared weights), node-wise multi-horizon readouts and exact BPTT.
//!
//!   a_t     = b + Win[kind(i)] . u_{t,i} (E units) + sum_e w_e r_{t-delay_e}[pre_e]
//!   x_{t+1} = (1-alpha) x_t + alpha a_t ,  r = tanh(x)
//!   y_{t,i} = Wout[kind(i)] . (r_E[i], r_I[i]) + bout[kind(i)]
use hcrnn::{optim::Adam, rng::Rng, Calibration, Connectome, ConnectomeRnn, RnnConfig};
use serde::{Deserialize, Serialize};

fn softplus(x: f64) -> f64 {
    if x > 30.0 { x } else { x.exp().ln_1p() }
}
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CirGnn {
    pub r: usize,
    pub kinds: Vec<usize>,
    pub n_kinds: usize,
    pub fin: usize,
    pub ko: usize,
    /// (post unit, pre unit, delay in steps)
    pub edges: Vec<[usize; 3]>,
    pub alpha: f64,
    pub params: Vec<f64>,
    /// radius of the delayed linearised map at initialisation (Arnoldi), for the record
    pub init_radius: f64,
    /// >0: residual readout. Each horizon head predicts the change from the node's current observed
    /// value (so 'same as now' is the baseline the network must improve on); value = channels per head.
    #[serde(default)]
    pub skip_nch: usize,
}

pub struct Layout {
    pub b: usize,
    pub win: usize,
    pub wout: usize,
    pub bout: usize,
    pub total: usize,
}

impl CirGnn {
    pub fn n(&self) -> usize {
        2 * self.r
    }
    pub fn layout(&self) -> Layout {
        let ne = self.edges.len();
        let b = ne;
        let win = b + self.n();
        let wout = win + self.n_kinds * self.fin;
        let bout = wout + self.n_kinds * self.ko * 2;
        Layout { b, win, wout, bout, total: bout + self.n_kinds * self.ko }
    }
    pub fn max_delay(&self) -> usize {
        self.edges.iter().map(|e| e[2]).max().unwrap_or(0)
    }

    /// Wire the network from a connectome using hcrnn (masks, delays, Dale's law, Arnoldi calibration).
    pub fn from_connectome(conn: &Connectome, kinds: Vec<usize>, n_kinds: usize, fin: usize, ko: usize, seed: u64) -> Result<Self, String> {
        let r = conn.n();
        if kinds.len() != r || kinds.iter().any(|&k| k >= n_kinds) {
            return Err("kinds must give one valid kind per node".into());
        }
        let cfg = RnnConfig {
            dt_ms: 1.0,
            tau_ms: 2.0,
            velocity_mm_per_ms: 1.0,
            calibration: Calibration::Arnoldi,
            target_radius: 0.9,
            input_regions: vec![0],
            output_regions: vec![0],
            n_in: 1,
            n_out: 1,
            seed,
            ..Default::default()
        };
        let net = ConnectomeRnn::new(conn, cfg)?;
        let ne = net.edges.len();
        let theta = net.params[net.layout.theta..net.layout.theta + ne].to_vec();
        let edges: Vec<[usize; 3]> = net.edges.iter().map(|e| [e.post, e.pre, e.delay]).collect();
        let init_radius = net.spectral_radius();
        let mut m = CirGnn { r, kinds, n_kinds, fin, ko, edges, alpha: net.alpha, params: vec![], init_radius, skip_nch: 0 };
        let l = m.layout();
        let mut p = vec![0.0; l.total];
        p[..ne].copy_from_slice(&theta);
        let mut rng = Rng::new(seed ^ 0xC1_46);
        let s_in = 1.0 / (fin as f64).sqrt();
        for k in 0..n_kinds * fin {
            p[l.win + k] = s_in * rng.normal();
        }
        for k in 0..n_kinds * ko * 2 {
            p[l.wout + k] = 0.3 * rng.normal();
        }
        m.params = p;
        Ok(m)
    }

    pub fn effective_weights(&self) -> Vec<f64> {
        self.edges
            .iter()
            .enumerate()
            .map(|(k, e)| if e[1] < self.r { softplus(self.params[k]) } else { -softplus(self.params[k]) })
            .collect()
    }

    /// Dale's law and wiring check: every pre-synaptic I unit has a negative weight, E positive.
    pub fn check_dale(&self) -> bool {
        self.effective_weights().iter().zip(&self.edges).all(|(w, e)| (e[1] < self.r) == (*w > 0.0))
    }
}

/// Incremental rollout (used for training, inference and free-running digital-twin simulation).
#[derive(Clone)]
pub struct Session {
    pub t: usize,
    /// (t+1) x N rates, row 0 = zeros
    pub r_all: Vec<f64>,
    x: Vec<f64>,
}

impl Session {
    pub fn new(n: usize) -> Self {
        Session { t: 0, r_all: vec![0.0; n], x: vec![0.0; n] }
    }
    pub fn rates(&self, n: usize) -> &[f64] {
        &self.r_all[self.t * n..(self.t + 1) * n]
    }
    /// Consume one input (R x fin, node-major) and return the readout (R x ko).
    pub fn step(&mut self, m: &CirGnn, w: &[f64], input: &[f64]) -> Vec<f64> {
        let n = m.n();
        let l = m.layout();
        let mut a: Vec<f64> = m.params[l.b..l.b + n].to_vec();
        for i in 0..m.r {
            let k = m.kinds[i];
            let mut s = 0.0;
            for c in 0..m.fin {
                s += m.params[l.win + k * m.fin + c] * input[i * m.fin + c];
            }
            a[i] += s;
        }
        for (ei, e) in m.edges.iter().enumerate() {
            if self.t >= e[2] {
                a[e[0]] += w[ei] * self.r_all[(self.t - e[2]) * n + e[1]];
            }
        }
        let mut rn = vec![0.0; n];
        for i in 0..n {
            self.x[i] = (1.0 - m.alpha) * self.x[i] + m.alpha * a[i];
            rn[i] = self.x[i].tanh();
        }
        self.r_all.extend_from_slice(&rn);
        self.t += 1;
        let mut y = vec![0.0; m.r * m.ko];
        for i in 0..m.r {
            let k = m.kinds[i];
            for o in 0..m.ko {
                let base = l.wout + (k * m.ko + o) * 2;
                let mut v = m.params[l.bout + k * m.ko + o] + m.params[base] * rn[i] + m.params[base + 1] * rn[m.r + i];
                if m.skip_nch > 0 {
                    let c = o % m.skip_nch;
                    v += input[i * m.fin + c] * input[i * m.fin + m.skip_nch + c];
                }
                y[i * m.ko + o] = v;
            }
        }
        y
    }
}

pub struct Window {
    pub t: usize,
    pub inputs: Vec<f64>,
    pub targets: Vec<f64>,
    pub weight: Vec<f64>,
}

impl CirGnn {
    pub fn forward(&self, w: &[f64], inputs: &[f64], t_len: usize) -> (Session, Vec<f64>) {
        let mut s = Session::new(self.n());
        let stride = self.r * self.fin;
        let mut ys = Vec::with_capacity(t_len * self.r * self.ko);
        for t in 0..t_len {
            let y = s.step(self, w, &inputs[t * stride..(t + 1) * stride]);
            ys.extend(y);
        }
        (s, ys)
    }

    /// Loss of one window and exact BPTT gradient accumulated into `grad`.
    pub fn loss_and_grad(&self, w: &[f64], win: &Window, grad: &mut [f64]) -> f64 {
        let n = self.n();
        let l = self.layout();
        let (sess, y) = self.forward(w, &win.inputs, win.t);
        let tot: f64 = win.weight.iter().sum();
        if tot <= 0.0 {
            return 0.0;
        }
        let norm = 1.0 / tot;
        let mut loss = 0.0;
        let mut gr = vec![0.0; (win.t + 1) * n];
        for t in 0..win.t {
            for i in 0..self.r {
                let k = self.kinds[i];
                for o in 0..self.ko {
                    let idx = (t * self.r + i) * self.ko + o;
                    let wt = win.weight[idx];
                    if wt == 0.0 {
                        continue;
                    }
                    let e = y[idx] - win.targets[idx];
                    loss += wt * e * e * norm;
                    let dy = 2.0 * wt * e * norm;
                    let base = l.wout + (k * self.ko + o) * 2;
                    grad[l.bout + k * self.ko + o] += dy;
                    grad[base] += dy * sess.r_all[(t + 1) * n + i];
                    grad[base + 1] += dy * sess.r_all[(t + 1) * n + self.r + i];
                    gr[(t + 1) * n + i] += dy * self.params[base];
                    gr[(t + 1) * n + self.r + i] += dy * self.params[base + 1];
                }
            }
        }
        let dw: Vec<f64> = self
            .edges
            .iter()
            .enumerate()
            .map(|(k, e)| {
                let s = sigmoid(self.params[k]);
                if e[1] < self.r { s } else { -s }
            })
            .collect();
        let mut gx_next = vec![0.0; n];
        let mut gx = vec![0.0; n];
        let mut ga = vec![0.0; n];
        for s in (1..=win.t).rev() {
            for i in 0..n {
                let rv = sess.r_all[s * n + i];
                gx[i] = gr[s * n + i] * (1.0 - rv * rv) + (1.0 - self.alpha) * gx_next[i];
                ga[i] = self.alpha * gx[i];
                grad[l.b + i] += ga[i];
            }
            let t = s - 1;
            for i in 0..self.r {
                let k = self.kinds[i];
                for c in 0..self.fin {
                    grad[l.win + k * self.fin + c] += ga[i] * win.inputs[(t * self.r + i) * self.fin + c];
                }
            }
            for (ei, e) in self.edges.iter().enumerate() {
                if t >= e[2] {
                    let sp = t - e[2];
                    let g = ga[e[0]];
                    grad[ei] += g * sess.r_all[sp * n + e[1]] * dw[ei];
                    gr[sp * n + e[1]] += w[ei] * g;
                }
            }
            std::mem::swap(&mut gx, &mut gx_next);
        }
        loss
    }

    pub fn batch_grad(&self, wins: &[Window], grad: &mut [f64], threads: usize) -> f64 {
        grad.iter_mut().for_each(|g| *g = 0.0);
        if wins.is_empty() {
            return 0.0;
        }
        let w = self.effective_weights();
        let np = self.params.len();
        let nt = threads.max(1).min(wins.len());
        let chunk = wins.len().div_ceil(nt);
        let res: Vec<(f64, Vec<f64>)> = std::thread::scope(|s| {
            let hs: Vec<_> = wins
                .chunks(chunk)
                .map(|ch| {
                    let w = &w;
                    s.spawn(move || {
                        let mut g = vec![0.0; np];
                        let mut l = 0.0;
                        for win in ch {
                            l += self.loss_and_grad(w, win, &mut g);
                        }
                        (l, g)
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("worker panicked")).collect()
        });
        let inv = 1.0 / wins.len() as f64;
        let mut loss = 0.0;
        for (l, g) in res {
            loss += l;
            for i in 0..np {
                grad[i] += g[i];
            }
        }
        grad.iter_mut().for_each(|g| *g *= inv);
        loss * inv
    }
}

// ---- data ------------------------------------------------------------------------------------------

/// Weekly standardised node x channel tensor with observation flags.
#[derive(Clone, Debug)]
pub struct Dataset {
    pub t: usize,
    pub r: usize,
    pub nch: usize,
    pub horizons: Vec<usize>,
    pub z: Vec<f64>,
    pub obs: Vec<f64>,
}

impl Dataset {
    pub fn fin(&self) -> usize {
        2 * self.nch
    }
    pub fn ko(&self) -> usize {
        self.nch * self.horizons.len()
    }
    pub fn input_at(&self, t: usize, out: &mut Vec<f64>) {
        for i in 0..self.r {
            let b = (t * self.r + i) * self.nch;
            out.extend_from_slice(&self.z[b..b + self.nch]);
            out.extend_from_slice(&self.obs[b..b + self.nch]);
        }
    }
    /// Window over steps [s, s+len). Targets count only where the target week lies in [lo, hi) and t >= burn.
    pub fn window(&self, s: usize, len: usize, lo: usize, hi: usize, burn: usize) -> Window {
        let (r, nch, ko) = (self.r, self.nch, self.ko());
        let mut inputs = Vec::with_capacity(len * r * self.fin());
        let mut targets = vec![0.0; len * r * ko];
        let mut weight = vec![0.0; len * r * ko];
        for t in 0..len {
            self.input_at(s + t, &mut inputs);
            for (hi_, &h) in self.horizons.iter().enumerate() {
                let tt = s + t + h;
                if t < burn || tt < lo || tt >= hi || tt >= self.t {
                    continue;
                }
                for i in 0..r {
                    for c in 0..nch {
                        let src = (tt * r + i) * nch + c;
                        if self.obs[src] > 0.0 {
                            let idx = (t * r + i) * ko + hi_ * nch + c;
                            targets[idx] = self.z[src];
                            weight[idx] = 1.0;
                        }
                    }
                }
            }
        }
        Window { t: len, inputs, targets, weight }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ChannelEval {
    pub mse: f64,
    pub persistence_mse: f64,
    pub mean_mse: f64,
    /// 1 - mse / persistence_mse (positive = beats "next week = this week")
    pub skill_vs_persistence: f64,
    pub skill_vs_mean: f64,
    pub n: usize,
    /// residual sd per horizon (z units)
    pub resid_sd: Vec<f64>,
}

/// Evaluate on targets in [lo, T): one long window so recurrent state is warm.
pub fn evaluate(m: &CirGnn, ds: &Dataset, lo: usize) -> Vec<ChannelEval> {
    let w = ds.window(0, ds.t, lo, ds.t, 0);
    let (_, y) = m.forward(&m.effective_weights(), &w.inputs, w.t);
    let (r, nch, ko, nh) = (ds.r, ds.nch, ds.ko(), ds.horizons.len());
    let mut out = vec![ChannelEval { resid_sd: vec![0.0; nh], ..Default::default() }; nch];
    let mut sse_h = vec![vec![0.0; nh]; nch];
    let mut cnt_h = vec![vec![0usize; nh]; nch];
    let (mut sm, mut sp, mut s0) = (vec![0.0; nch], vec![0.0; nch], vec![0.0; nch]);
    for t in 0..w.t {
        for i in 0..r {
            for h in 0..nh {
                for c in 0..nch {
                    let idx = (t * r + i) * ko + h * nch + c;
                    if w.weight[idx] == 0.0 {
                        continue;
                    }
                    let tgt = w.targets[idx];
                    let e = y[idx] - tgt;
                    let pers = if ds.obs[(t * r + i) * nch + c] > 0.0 { ds.z[(t * r + i) * nch + c] } else { 0.0 };
                    sm[c] += e * e;
                    sp[c] += (pers - tgt) * (pers - tgt);
                    s0[c] += tgt * tgt;
                    sse_h[c][h] += e * e;
                    cnt_h[c][h] += 1;
                    out[c].n += 1;
                }
            }
        }
    }
    for c in 0..nch {
        let n = out[c].n.max(1) as f64;
        out[c].mse = sm[c] / n;
        out[c].persistence_mse = sp[c] / n;
        out[c].mean_mse = s0[c] / n;
        out[c].skill_vs_persistence = if sp[c] > 0.0 { 1.0 - sm[c] / sp[c] } else { 0.0 };
        out[c].skill_vs_mean = if s0[c] > 0.0 { 1.0 - sm[c] / s0[c] } else { 0.0 };
        for h in 0..nh {
            out[c].resid_sd[h] = (sse_h[c][h] / cnt_h[c][h].max(1) as f64).sqrt();
        }
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FitConfig {
    pub steps: usize,
    pub batch: usize,
    pub lr: f64,
    pub window: usize,
    pub burn: usize,
    pub holdout: usize,
    pub seed: u64,
    pub threads: usize,
    pub eval_every: usize,
}

impl Default for FitConfig {
    fn default() -> Self {
        FitConfig { steps: 300, batch: 12, lr: 4e-3, window: 26, burn: 4, holdout: 12, seed: 1, threads: 4, eval_every: 25 }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FitReport {
    pub steps: usize,
    pub best_step: usize,
    pub train_loss_first: f64,
    pub train_loss_last: f64,
    pub val_loss_best: f64,
    pub holdout_weeks: usize,
    pub per_channel: Vec<ChannelEval>,
    pub seconds: f64,
}

/// Train on weeks [0, T-holdout); keep the checkpoint with the best holdout loss. When there is no
/// holdout (tiny histories) the last checkpoint is kept and the report says so.
pub fn fit(m: &mut CirGnn, ds: &Dataset, cfg: &FitConfig) -> Result<FitReport, String> {
    let maxh = *ds.horizons.iter().max().unwrap_or(&1);
    let hold = cfg.holdout.min(ds.t / 3);
    let t_train = ds.t - hold;
    let wlen = cfg.window.min(t_train.saturating_sub(maxh + 1));
    if wlen <= cfg.burn + 2 {
        return Err(format!("not enough history: {} weeks usable, need more than {}", t_train, cfg.burn + maxh + 4));
    }
    let start = std::time::Instant::now();
    let mut rng = Rng::new(cfg.seed);
    let mut opt = Adam::new(m.params.len(), cfg.lr, 1.0);
    let mut grad = vec![0.0; m.params.len()];
    let (mut best, mut best_params, mut best_step) = (f64::INFINITY, m.params.clone(), 0usize);
    let (mut first, mut last) = (f64::NAN, f64::NAN);
    for step in 1..=cfg.steps {
        let wins: Vec<Window> = (0..cfg.batch)
            .map(|_| {
                let s = rng.below(t_train - wlen - maxh + 1);
                ds.window(s, wlen, 0, t_train, cfg.burn)
            })
            .collect();
        let loss = m.batch_grad(&wins, &mut grad, cfg.threads);
        if first.is_nan() {
            first = loss;
        }
        last = loss;
        opt.step(&mut m.params, &grad);
        if hold > 0 && (step % cfg.eval_every == 0 || step == cfg.steps) {
            let ev = evaluate(m, ds, t_train);
            let v: f64 = ev.iter().map(|e| e.mse).sum::<f64>() / ev.len() as f64;
            if v < best {
                best = v;
                best_params.copy_from_slice(&m.params);
                best_step = step;
            }
        }
    }
    if hold > 0 {
        m.params.copy_from_slice(&best_params);
    } else {
        best_step = cfg.steps;
    }
    let per_channel = evaluate(m, ds, if hold > 0 { t_train } else { cfg.burn });
    Ok(FitReport {
        steps: cfg.steps,
        best_step,
        train_loss_first: first,
        train_loss_last: last,
        val_loss_best: if best.is_finite() { best } else { f64::NAN },
        holdout_weeks: hold,
        per_channel,
        seconds: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hcrnn::Region;

    fn ring(n: usize) -> Connectome {
        let regions: Vec<Region> = (0..n).map(|i| Region { name: format!("n{i}"), hemi: '?', system: if i % 2 == 0 { "A".into() } else { "B".into() }, pos: [0.0; 3] }).collect();
        let (mut w, mut l) = (vec![0.0; n * n], vec![0.0; n * n]);
        for i in 0..n {
            for d in [1usize, 2] {
                let j = (i + d) % n;
                w[i * n + j] = 1.0 / d as f64;
                l[i * n + j] = d as f64;
            }
        }
        Connectome::new(regions, w, l, "test").unwrap()
    }

    #[test]
    fn gradient_matches_finite_differences() {
        let conn = ring(6);
        let kinds: Vec<usize> = (0..6).map(|i| i % 2).collect();
        let mut m = CirGnn::from_connectome(&conn, kinds, 2, 4, 3, 7).unwrap();
        assert!(m.check_dale());
        let mut rng = Rng::new(3);
        // non-trivial parameters everywhere
        for p in m.params.iter_mut() {
            *p += 0.1 * rng.normal();
        }
        let t = 12;
        let win = Window {
            t,
            inputs: (0..t * 6 * 4).map(|_| rng.normal()).collect(),
            targets: (0..t * 6 * 3).map(|_| rng.normal()).collect(),
            weight: (0..t * 6 * 3).map(|_| if rng.uniform() < 0.7 { 1.0 } else { 0.0 }).collect(),
        };
        let mut g = vec![0.0; m.params.len()];
        m.loss_and_grad(&m.effective_weights(), &win, &mut g);
        let mut worst = 0.0f64;
        for k in (0..m.params.len()).step_by(3) {
            let eps = 1e-6;
            let orig = m.params[k];
            m.params[k] = orig + eps;
            let mut z = vec![0.0; g.len()];
            let lp = m.loss_and_grad(&m.effective_weights(), &win, &mut z);
            m.params[k] = orig - eps;
            let lm = m.loss_and_grad(&m.effective_weights(), &win, &mut z);
            m.params[k] = orig;
            let fd = (lp - lm) / (2.0 * eps);
            let rel = (fd - g[k]).abs() / (fd.abs().max(g[k].abs()).max(1e-6));
            worst = worst.max(rel.min((fd - g[k]).abs() * 1e4));
        }
        assert!(worst < 1e-4, "worst relative gradient error {worst}");
    }

    #[test]
    fn learns_delayed_graph_propagation_better_than_persistence() {
        // z_i(t+1) = 0.4 z_i(t) + 0.5 * mean_{j in {i-1,i-2}} z_j(t - d_j) + noise: needs the graph + memory.
        let n = 6;
        let conn = ring(n);
        let mut rng = Rng::new(11);
        let t_len = 160;
        let mut z = vec![0.0; t_len * n];
        for t in 1..t_len {
            for i in 0..n {
                let a = z[(t - 1) * n + (i + n - 1) % n];
                let b = if t >= 2 { z[(t - 2) * n + (i + n - 2) % n] } else { 0.0 };
                z[t * n + i] = 0.35 * z[(t - 1) * n + i] + 0.45 * (a + b) / 2.0 + 0.6 * rng.normal();
            }
        }
        let ds = Dataset { t: t_len, r: n, nch: 1, horizons: vec![1], z, obs: vec![1.0; t_len * n] };
        let kinds = vec![0; n];
        let mut m = CirGnn::from_connectome(&conn, kinds, 1, 2, 1, 5).unwrap();
        let cfg = FitConfig { steps: 220, batch: 8, lr: 6e-3, window: 24, burn: 4, holdout: 40, seed: 2, threads: 2, eval_every: 20 };
        let rep = fit(&mut m, &ds, &cfg).unwrap();
        let e = &rep.per_channel[0];
        assert!(m.check_dale());
        assert!(e.mse < e.mean_mse, "model {} vs mean {}", e.mse, e.mean_mse);
        assert!(e.mse < e.persistence_mse, "model {} vs persistence {}", e.mse, e.persistence_mse);
    }
}

//! Cluster analysis: k-means (Lloyd's, MacQueen's and Hartigan and Wong's algorithms),
//! agglomerative hierarchical clustering, and cutting the resulting tree.
//!
//! k-means follows Hartigan and Wong, "Algorithm AS 136: A K-Means Clustering Algorithm"
//! (Applied Statistics, 1979), and the hierarchical clustering Murtagh's nearest-neighbour
//! list algorithm with Lance and Williams' update formulae ("Multidimensional Clustering
//! Algorithms", 1985).

use {
    crate::{
        builtin_libraries::NativeCall,
        stats::{c_arguments, c_return, integer_values, matrix_shape},
        unwind::R,
        values::{self, symbols},
    },
    heap::object::{NA_INTEGER, Sexp},
};

/// `.C(kmeans_Lloyd, x, n, p, centers, k, cl, maxiter, nc, wss)`.
pub unsafe fn kmeans_lloyd(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let mut problem = KMeans::read(&arguments);
    let mut iteration = 0;
    while iteration < problem.max_iterations {
        let mut updated = false;
        for i in 0..problem.n {
            let nearest = problem.nearest(i) + 1;
            if problem.cluster[i] != nearest as i32 {
                updated = true;
                problem.cluster[i] = nearest as i32;
            }
        }
        if !updated {
            break;
        }
        problem.recompute_centres();
        iteration += 1;
    }
    problem.iterations = iteration + 1;
    problem.write(&arguments);
    c_return(call, &arguments)
}

/// `.C(kmeans_MacQueen, x, n, p, centers, k, cl, maxiter, nc, wss)`: each transfer moves the
/// two centres involved at once.
pub unsafe fn kmeans_macqueen(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let mut problem = KMeans::read(&arguments);
    for i in 0..problem.n {
        problem.cluster[i] = problem.nearest(i) as i32 + 1;
    }
    problem.recompute_centres();
    let (n, k, p) = (problem.n, problem.k, problem.p);
    let mut iteration = 0;
    while iteration < problem.max_iterations {
        let mut updated = false;
        for i in 0..n {
            let nearest = problem.nearest(i);
            let current = problem.cluster[i] as usize - 1;
            if current != nearest {
                updated = true;
                problem.cluster[i] = nearest as i32 + 1;
                problem.counts[current] -= 1;
                problem.counts[nearest] += 1;
                for c in 0..p {
                    let value = problem.x[i + n * c];
                    let old = &mut problem.centres[current + k * c];
                    *old += (*old - value) / problem.counts[current] as f64;
                    let new = &mut problem.centres[nearest + k * c];
                    *new += (value - *new) / problem.counts[nearest] as f64;
                }
            }
        }
        if !updated {
            break;
        }
        iteration += 1;
    }
    problem.iterations = iteration + 1;
    problem.write(&arguments);
    c_return(call, &arguments)
}

/// The shared state of Lloyd's and MacQueen's algorithms, read from the `.C` arguments.
struct KMeans {
    x: Vec<f64>,
    n: usize,
    p: usize,
    centres: Vec<f64>,
    k: usize,
    cluster: Vec<i32>,
    max_iterations: i32,
    iterations: i32,
    counts: Vec<i32>,
}

impl KMeans {
    unsafe fn read(arguments: &[Sexp]) -> KMeans {
        let n = values::as_integer_scalar(arguments[1]).max(0) as usize;
        let k = values::as_integer_scalar(arguments[4]).max(0) as usize;
        KMeans {
            x: values::reals(arguments[0]).to_vec(),
            n,
            p: values::as_integer_scalar(arguments[2]).max(0) as usize,
            centres: values::reals(arguments[3]).to_vec(),
            k,
            cluster: vec![-1; n],
            max_iterations: values::as_integer_scalar(arguments[6]),
            iterations: 0,
            counts: vec![0; k],
        }
    }

    fn nearest(&self, i: usize) -> usize {
        let mut best = f64::INFINITY;
        let mut nearest = 0;
        for j in 0..self.k {
            let mut distance = 0.0;
            for c in 0..self.p {
                let difference = self.x[i + self.n * c] - self.centres[j + self.k * c];
                distance += difference * difference;
            }
            if distance < best {
                best = distance;
                nearest = j;
            }
        }
        nearest
    }

    fn recompute_centres(&mut self) {
        self.centres.fill(0.0);
        self.counts.fill(0);
        for i in 0..self.n {
            let cluster = self.cluster[i] as usize - 1;
            self.counts[cluster] += 1;
            for c in 0..self.p {
                self.centres[cluster + c * self.k] += self.x[i + c * self.n];
            }
        }
        for j in 0..self.k * self.p {
            self.centres[j] /= self.counts[j % self.k] as f64;
        }
    }

    unsafe fn write(&self, arguments: &[Sexp]) {
        let mut sums = vec![0.0; self.k];
        for i in 0..self.n {
            let cluster = self.cluster[i] as usize - 1;
            for c in 0..self.p {
                let difference = self.x[i + self.n * c] - self.centres[cluster + self.k * c];
                sums[cluster] += difference * difference;
            }
        }
        values::reals_mut(arguments[3]).copy_from_slice(&self.centres);
        values::ints_mut(arguments[5]).copy_from_slice(&self.cluster);
        values::ints_mut(arguments[6])[0] = self.iterations;
        values::ints_mut(arguments[7]).copy_from_slice(&self.counts);
        values::reals_mut(arguments[8]).copy_from_slice(&sums);
    }
}

/// `.Fortran(kmns, a, m, n, c, k, ic1, ic2, nc, an1, an2, ncp, d, itran, live, iter, wss,
/// ifault)`: Hartigan and Wong's algorithm, alternating optimal transfer and quick transfer
/// stages until no point moves.
pub unsafe fn kmns(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let a = values::reals(arguments[0]).to_vec();
    let m = values::as_integer_scalar(arguments[1]).max(0) as usize;
    let n = values::as_integer_scalar(arguments[2]).max(0) as usize;
    let k = values::as_integer_scalar(arguments[4]).max(0) as usize;
    let max_iterations = values::as_integer_scalar(arguments[14]);
    let max_quick_steps = values::ints(arguments[12]).first().copied().unwrap_or(0);
    let mut state = HartiganWong {
        a,
        m,
        n,
        k,
        c: values::reals(arguments[3]).to_vec(),
        ic1: vec![0; m],
        ic2: vec![0; m],
        nc: vec![0; k],
        an1: vec![0.0; k],
        an2: vec![0.0; k],
        ncp: vec![0; k],
        d: vec![0.0; m],
        itran: vec![0; k],
        live: vec![0; k],
        index: 0,
        max_quick_steps,
    };
    let (iterations, fault) = state.run(max_iterations);
    let mut sums = vec![0.0; k];
    if fault != 1 && fault != 3 {
        state.c.fill(0.0);
        for i in 0..m {
            let cluster = state.ic1[i];
            for j in 0..n {
                state.c[cluster + k * j] += state.a[i + m * j];
            }
        }
        for j in 0..n {
            for l in 0..k {
                state.c[l + k * j] /= state.nc[l] as f64;
            }
            for i in 0..m {
                let cluster = state.ic1[i];
                let difference = state.a[i + m * j] - state.c[cluster + k * j];
                sums[cluster] += difference * difference;
            }
        }
    }
    values::reals_mut(arguments[3]).copy_from_slice(&state.c);
    let one_based =
        |indices: &[usize]| -> Vec<i32> { indices.iter().map(|&index| index as i32 + 1).collect() };
    values::ints_mut(arguments[5]).copy_from_slice(&one_based(&state.ic1));
    values::ints_mut(arguments[6]).copy_from_slice(&one_based(&state.ic2));
    values::ints_mut(arguments[7]).copy_from_slice(&state.nc);
    values::reals_mut(arguments[8]).copy_from_slice(&state.an1);
    values::reals_mut(arguments[9]).copy_from_slice(&state.an2);
    let narrow = |steps: &[i64]| -> Vec<i32> { steps.iter().map(|&step| step as i32).collect() };
    values::ints_mut(arguments[10]).copy_from_slice(&narrow(&state.ncp));
    values::reals_mut(arguments[11]).copy_from_slice(&state.d);
    let transfers = values::ints_mut(arguments[12]);
    transfers[..k].copy_from_slice(&state.itran);
    if state.max_quick_steps < 0 {
        transfers[0] = -1;
    }
    values::ints_mut(arguments[13]).copy_from_slice(&narrow(&state.live));
    values::ints_mut(arguments[14])[0] = iterations;
    values::reals_mut(arguments[15]).copy_from_slice(&sums);
    values::ints_mut(arguments[16])[0] = fault;
    c_return(call, &arguments)
}

/// Hartigan and Wong's working state; indices are 0-based, `live` and `ncp` hold the 1-based
/// step numbers AS 136 compares against.
struct HartiganWong {
    a: Vec<f64>,
    m: usize,
    n: usize,
    k: usize,
    c: Vec<f64>,
    ic1: Vec<usize>,
    ic2: Vec<usize>,
    nc: Vec<i32>,
    an1: Vec<f64>,
    an2: Vec<f64>,
    ncp: Vec<i64>,
    d: Vec<f64>,
    itran: Vec<i32>,
    live: Vec<i64>,
    /// Steps since the last transfer.
    index: usize,
    /// The quick transfer stage's step limit, set to -1 once it is reached.
    max_quick_steps: i32,
}

const BIG: f64 = 1.0e30;

impl HartiganWong {
    fn distance(&self, i: usize, l: usize) -> f64 {
        let mut total = 0.0;
        for j in 0..self.n {
            let difference = self.a[i + self.m * j] - self.c[l + self.k * j];
            total += difference * difference;
        }
        total
    }

    /// Returns the iterations used and the fault code (0, 1 for an empty cluster, 2 for no
    /// convergence, 3 for an invalid `k`, 4 when the quick transfer stage ran too long).
    fn run(&mut self, max_iterations: i32) -> (i32, i32) {
        let (m, n, k) = (self.m, self.n, self.k);
        if k <= 1 || k >= m {
            return (0, 3);
        }
        for i in 0..m {
            self.ic1[i] = 0;
            self.ic2[i] = 1;
            let mut best = [self.distance(i, 0), self.distance(i, 1)];
            if best[0] > best[1] {
                self.ic1[i] = 1;
                self.ic2[i] = 0;
                best.swap(0, 1);
            }
            for l in 2..k {
                let mut db = 0.0;
                let mut far = false;
                for j in 0..n {
                    let difference = self.a[i + m * j] - self.c[l + k * j];
                    db += difference * difference;
                    if db >= best[1] {
                        far = true;
                        break;
                    }
                }
                if far {
                    continue;
                }
                if db < best[0] {
                    best[1] = best[0];
                    self.ic2[i] = self.ic1[i];
                    best[0] = db;
                    self.ic1[i] = l;
                } else {
                    best[1] = db;
                    self.ic2[i] = l;
                }
            }
        }
        self.c.fill(0.0);
        self.nc.fill(0);
        for i in 0..m {
            let l = self.ic1[i];
            self.nc[l] += 1;
            for j in 0..n {
                self.c[l + k * j] += self.a[i + m * j];
            }
        }
        for l in 0..k {
            if self.nc[l] == 0 {
                return (0, 1);
            }
            let size = self.nc[l] as f64;
            for j in 0..n {
                self.c[l + k * j] /= size;
            }
            self.an2[l] = size / (size + 1.0);
            self.an1[l] = if size > 1.0 { size / (size - 1.0) } else { BIG };
            self.itran[l] = 1;
            self.ncp[l] = -1;
        }
        self.index = 0;
        for iteration in 1..=max_iterations.max(0) {
            self.optimal_transfer();
            if self.index == m {
                return (iteration, 0);
            }
            self.quick_transfer();
            if self.max_quick_steps < 0 {
                return (iteration, 4);
            }
            if k == 2 {
                return (iteration, 0);
            }
            self.ncp.fill(0);
        }
        (max_iterations + 1, 2)
    }

    /// Moves `i` from cluster `from` to cluster `to`, updating both centres and their
    /// size factors.
    fn transfer(&mut self, i: usize, from: usize, to: usize) {
        let (m, k) = (self.m, self.k);
        let al1 = self.nc[from] as f64;
        let alw = al1 - 1.0;
        let al2 = self.nc[to] as f64;
        let alt = al2 + 1.0;
        for j in 0..self.n {
            let value = self.a[i + m * j];
            self.c[from + k * j] = (self.c[from + k * j] * al1 - value) / alw;
            self.c[to + k * j] = (self.c[to + k * j] * al2 + value) / alt;
        }
        self.nc[from] -= 1;
        self.nc[to] += 1;
        self.an2[from] = alw / al1;
        self.an1[from] = if alw > 1.0 { alw / (alw - 1.0) } else { BIG };
        self.an1[to] = alt / al2;
        self.an2[to] = alt / (alt + 1.0);
        self.ic1[i] = to;
        self.ic2[i] = from;
    }

    /// AS 136's OPTRA: for each point, the transfer to the cluster that most reduces the
    /// within-cluster sum of squares, among the live clusters.
    fn optimal_transfer(&mut self) {
        let (m, k) = (self.m, self.k);
        let steps = m as i64;
        for l in 0..k {
            if self.itran[l] == 1 {
                self.live[l] = steps + 1;
            }
        }
        for i in 0..m {
            let step = i as i64 + 1;
            self.index += 1;
            let l1 = self.ic1[i];
            if self.nc[l1] != 1 {
                if self.ncp[l1] != 0 {
                    self.d[i] = self.distance(i, l1) * self.an1[l1];
                }
                let first_other = self.ic2[i];
                let mut l2 = first_other;
                let mut r2 = self.distance(i, l2) * self.an2[l2];
                for l in 0..k {
                    if (step >= self.live[l1] && step >= self.live[l])
                        || l == l1
                        || l == first_other
                    {
                        continue;
                    }
                    let rr = r2 / self.an2[l];
                    let mut dc = 0.0;
                    let mut far = false;
                    for j in 0..self.n {
                        let difference = self.a[i + m * j] - self.c[l + k * j];
                        dc += difference * difference;
                        if dc >= rr {
                            far = true;
                            break;
                        }
                    }
                    if far {
                        continue;
                    }
                    r2 = dc * self.an2[l];
                    l2 = l;
                }
                if r2 >= self.d[i] {
                    self.ic2[i] = l2;
                } else {
                    self.index = 0;
                    self.live[l1] = steps + step;
                    self.live[l2] = steps + step;
                    self.ncp[l1] = step;
                    self.ncp[l2] = step;
                    self.transfer(i, l1, l2);
                }
            }
            if self.index == m {
                return;
            }
        }
        for l in 0..k {
            self.itran[l] = 0;
            self.live[l] -= steps;
        }
    }

    /// AS 136's QTRAN: swaps each point between its closest and second closest clusters when
    /// that helps, until a full pass over the points moves none.
    fn quick_transfer(&mut self) {
        let (m, k) = (self.m, self.k);
        let steps = m as i64;
        let mut count = 0usize;
        let mut step: i64 = 0;
        loop {
            for i in 0..m {
                count += 1;
                step += 1;
                if step >= self.max_quick_steps as i64 {
                    self.max_quick_steps = -1;
                    return;
                }
                let l1 = self.ic1[i];
                let l2 = self.ic2[i];
                if self.nc[l1] != 1 {
                    if step <= self.ncp[l1] {
                        self.d[i] = self.distance(i, l1) * self.an1[l1];
                    }
                    if step < self.ncp[l1] || step < self.ncp[l2] {
                        let r2 = self.d[i] / self.an2[l2];
                        let mut dd = 0.0;
                        let mut far = false;
                        for j in 0..self.n {
                            let difference = self.a[i + m * j] - self.c[l2 + k * j];
                            dd += difference * difference;
                            if dd >= r2 {
                                far = true;
                                break;
                            }
                        }
                        if !far {
                            count = 0;
                            self.index = 0;
                            self.itran[l1] = 1;
                            self.itran[l2] = 1;
                            self.ncp[l1] = step + steps;
                            self.ncp[l2] = step + steps;
                            self.transfer(i, l1, l2);
                        }
                    }
                }
                if count == m {
                    return;
                }
            }
        }
    }
}

/// `.Fortran(hclust, n, len, method, ia, ib, crit, members, nn, disnn, diss)`: agglomerates
/// the closest pair of clusters `n - 1` times, keeping each cluster's nearest neighbour to its
/// right. Methods: 1 Ward (on the given dissimilarities), 2 single, 3 complete, 4 average,
/// 5 McQuitty, 6 median, 7 centroid, 8 Ward on squared dissimilarities.
pub unsafe fn hclust(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let n = values::as_integer_scalar(arguments[0]).max(0) as usize;
    let method = values::as_integer_scalar(arguments[2]);
    let mut members = values::reals(arguments[6]).to_vec();
    let mut dissimilarity = values::reals(arguments[9]).to_vec();
    // The index of the pair `i < j` in the packed lower triangle `dist` stores.
    let offset = |i: usize, j: usize| j + i * n - (i + 1) * (i + 2) / 2;
    let infinity = 1.0e300;
    let mut active = vec![true; n];
    let mut neighbour = vec![0usize; n];
    let mut neighbour_distance = vec![0.0; n];
    let mut first = vec![0i32; n];
    let mut second = vec![0i32; n];
    let mut criterion = vec![0.0; n];
    if method == 8 {
        for value in dissimilarity.iter_mut() {
            *value *= *value;
        }
    }
    for i in 0..n.saturating_sub(1) {
        let mut best = infinity;
        let mut closest = 0;
        for j in i + 1..n {
            let value = dissimilarity[offset(i, j)];
            if best > value {
                best = value;
                closest = j;
            }
        }
        neighbour[i] = closest;
        neighbour_distance[i] = best;
    }
    let mut clusters = n;
    while clusters > 1 {
        let mut best = infinity;
        let (mut im, mut jm) = (0, 0);
        for i in 0..n - 1 {
            if active[i] && neighbour_distance[i] < best {
                best = neighbour_distance[i];
                im = i;
                jm = neighbour[i];
            }
        }
        clusters -= 1;
        let i2 = im.min(jm);
        let j2 = im.max(jm);
        let step = n - clusters - 1;
        first[step] = i2 as i32 + 1;
        second[step] = j2 as i32 + 1;
        criterion[step] = if method == 8 { best.sqrt() } else { best };
        active[j2] = false;
        let mut nearest_distance = infinity;
        let mut nearest = 0;
        let joined = dissimilarity[offset(i2, j2)];
        for k in 0..n {
            if !active[k] || k == i2 {
                continue;
            }
            let index1 = if i2 < k { offset(i2, k) } else { offset(k, i2) };
            let index2 = if j2 < k { offset(j2, k) } else { offset(k, j2) };
            let (d1, d2) = (dissimilarity[index1], dissimilarity[index2]);
            let (mi, mj, mk) = (members[i2], members[j2], members[k]);
            dissimilarity[index1] = match method {
                1 | 8 => ((mi + mk) * d1 + (mj + mk) * d2 - mk * joined) / (mi + mj + mk),
                2 => d1.min(d2),
                3 => d1.max(d2),
                4 => (mi * d1 + mj * d2) / (mi + mj),
                5 => (d1 + d2) / 2.0,
                6 => ((d1 + d2) - joined / 2.0) / 2.0,
                _ => (mi * d1 + mj * d2 - mi * mj * joined / (mi + mj)) / (mi + mj),
            };
            if i2 < k {
                if dissimilarity[index1] < nearest_distance {
                    nearest_distance = dissimilarity[index1];
                    nearest = k;
                }
            } else if dissimilarity[index1] < neighbour_distance[k] {
                // A cluster to the left may now have the merged cluster as its nearest
                // neighbour, which the non-monotone methods need.
                neighbour_distance[k] = dissimilarity[index1];
                neighbour[k] = i2;
            }
        }
        members[i2] += members[j2];
        neighbour_distance[i2] = nearest_distance;
        neighbour[i2] = nearest;
        for i in 0..n - 1 {
            if active[i] && (neighbour[i] == i2 || neighbour[i] == j2) {
                let mut best = infinity;
                for j in i + 1..n {
                    if active[j] {
                        let value = dissimilarity[offset(i, j)];
                        if value < best {
                            best = value;
                            nearest = j;
                        }
                    }
                }
                neighbour[i] = nearest;
                neighbour_distance[i] = best;
            }
        }
    }
    values::ints_mut(arguments[3]).copy_from_slice(&first);
    values::ints_mut(arguments[4]).copy_from_slice(&second);
    values::reals_mut(arguments[5]).copy_from_slice(&criterion);
    values::reals_mut(arguments[6]).copy_from_slice(&members);
    let one_based: Vec<i32> = neighbour.iter().map(|&index| index as i32 + 1).collect();
    values::ints_mut(arguments[7]).copy_from_slice(&one_based);
    values::reals_mut(arguments[8]).copy_from_slice(&neighbour_distance);
    values::reals_mut(arguments[9]).copy_from_slice(&dissimilarity);
    c_return(call, &arguments)
}

/// `.Fortran(hcass2, n, ia, ib, order, iia, iib)`: converts `hclust`'s merge pairs (cluster
/// representatives) to R's convention (negative observations, positive earlier merges, with
/// observations first) and computes the leaf order of the dendrogram.
pub unsafe fn hcass2(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let n = values::as_integer_scalar(arguments[0]).max(0) as usize;
    let ia = values::ints(arguments[1]).to_vec();
    let ib = values::ints(arguments[2]).to_vec();
    let mut iia = ia.clone();
    let mut iib = ib.clone();
    for i in 0..n.saturating_sub(2) {
        let representative = ia[i].min(ib[i]);
        for j in i + 1..n - 1 {
            if ia[j] == representative {
                iia[j] = -(i as i32 + 1);
            }
            if ib[j] == representative {
                iib[j] = -(i as i32 + 1);
            }
        }
    }
    for i in 0..n.saturating_sub(1) {
        iia[i] = -iia[i];
        iib[i] = -iib[i];
    }
    for i in 0..n.saturating_sub(1) {
        if iia[i] > 0 && iib[i] < 0 {
            std::mem::swap(&mut iia[i], &mut iib[i]);
        }
        if iia[i] > 0 && iib[i] > 0 {
            let (low, high) = (iia[i].min(iib[i]), iia[i].max(iib[i]));
            iia[i] = low;
            iib[i] = high;
        }
    }
    let mut order: Vec<i32> = Vec::with_capacity(n);
    if n >= 2 {
        order.push(iia[n - 2]);
        order.push(iib[n - 2]);
        for step in (1..n - 1).rev() {
            if let Some(position) = order.iter().position(|&entry| entry == step as i32) {
                order[position] = iia[step - 1];
                order.insert(position + 1, iib[step - 1]);
            }
        }
    }
    let order: Vec<i32> = order.iter().map(|&entry| -entry).collect();
    let slot = values::ints_mut(arguments[3]);
    slot[..order.len()].copy_from_slice(&order);
    values::ints_mut(arguments[4]).copy_from_slice(&iia);
    values::ints_mut(arguments[5]).copy_from_slice(&iib);
    c_return(call, &arguments)
}

/// `cutree(merge, which)`: the cluster of each observation when an `hclust` tree is cut into
/// each number of groups in `which`. Clusters are numbered in order of their first
/// observation.
pub unsafe fn cutree(call: &NativeCall<'_>) -> R<Sexp> {
    let merge = integer_values(call.arg(0))?;
    let which = integer_values(call.arg(1))?;
    let steps = matrix_shape(call.arg(0)).0;
    let n = steps + 1;
    let mut result = vec![0; n * which.len()];
    for (column, &groups) in which.iter().enumerate() {
        if groups == NA_INTEGER || groups < 1 || groups as usize > n {
            return Err(call.error(format!("elements of 'k' must be between 1 and {n}")));
        }
        let mut parent: Vec<usize> = (0..n).collect();
        fn root(parent: &mut [usize], mut node: usize) -> usize {
            while parent[node] != node {
                parent[node] = parent[parent[node]];
                node = parent[node];
            }
            node
        }
        // The cluster (as an observation in it) each merge step formed.
        let mut formed = vec![0usize; steps];
        for step in 0..n - groups as usize {
            let member = |entry: i32, formed: &[usize]| {
                if entry < 0 {
                    (-entry - 1) as usize
                } else {
                    formed[entry as usize - 1]
                }
            };
            let left = member(merge[step], &formed);
            let right = member(merge[step + steps], &formed);
            let (left_root, right_root) = (root(&mut parent, left), root(&mut parent, right));
            parent[right_root] = left_root;
            formed[step] = left_root;
        }
        let mut labels: Vec<Option<i32>> = vec![None; n];
        let mut next = 0;
        for observation in 0..n {
            let cluster = root(&mut parent, observation);
            let label = *labels[cluster].get_or_insert_with(|| {
                next += 1;
                next
            });
            result[observation + column * n] = label;
        }
    }
    let answer = values::integer_vector(&result)?;
    let dim = values::integer_vector(&[n as i32, which.len() as i32])?;
    values::set_attr(answer, symbols().dim, dim)?;
    Ok(answer)
}

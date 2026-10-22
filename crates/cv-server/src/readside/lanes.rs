//! History graph lanes, computed on the server so every client draws the same graph.
//!
//! Rows come in jj's grouped topological order (children before parents). Each lane holds the
//! commit it is waiting for; a row takes the lane that waits for it (or the first free one), its
//! first parent continues in that lane, and other parents reuse a lane already waiting for them or
//! open a new one.

/// Lane of each row and, per row, the lane each edge leads to.
pub fn assign(rows: &[(String, Vec<String>)]) -> Vec<(u32, Vec<u32>)> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    let free = |lanes: &mut Vec<Option<String>>| -> usize {
        match lanes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                lanes.push(None);
                lanes.len() - 1
            }
        }
    };
    for (id, parents) in rows {
        let me = match lanes.iter().position(|l| l.as_deref() == Some(id)) {
            Some(i) => i,
            None => free(&mut lanes),
        };
        // Every lane that waited for this commit merges here.
        for l in lanes.iter_mut() {
            if l.as_deref() == Some(id) {
                *l = None;
            }
        }
        let mut to = Vec::with_capacity(parents.len());
        for (k, p) in parents.iter().enumerate() {
            if let Some(j) = lanes.iter().position(|l| l.as_deref() == Some(p)) {
                to.push(j as u32);
            } else if k == 0 && lanes[me].is_none() {
                lanes[me] = Some(p.clone());
                to.push(me as u32);
            } else {
                let j = free(&mut lanes);
                lanes[j] = Some(p.clone());
                to.push(j as u32);
            }
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        out.push((me as u32, to));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::assign;

    fn r(id: &str, parents: &[&str]) -> (String, Vec<String>) {
        (id.into(), parents.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn stacks_branch_off_and_merge_back() {
        // s2 → s1 → t1 → t0, x1 → t0 (side branch), m (merge of t1 and x1) on top.
        let rows = [
            r("s2", &["s1"]),
            r("s1", &["t1"]),
            r("x1", &["t0"]),
            r("t1", &["t0"]),
            r("t0", &[]),
        ];
        let lanes = assign(&rows);
        assert_eq!(lanes[0], (0, vec![0]));
        assert_eq!(lanes[1], (0, vec![0]));
        assert_eq!(lanes[2], (1, vec![1]), "a second head opens lane 1");
        assert_eq!(lanes[3], (0, vec![1]), "t0 is already awaited in lane 1");
        assert_eq!(lanes[4].0, 1);

        let rows = [
            r("m", &["a", "b"]),
            r("b", &["c"]),
            r("a", &["c"]),
            r("c", &[]),
        ];
        let lanes = assign(&rows);
        assert_eq!(lanes[0], (0, vec![0, 1]));
        assert_eq!(lanes[1], (1, vec![1]));
        assert_eq!(lanes[2], (0, vec![1]));
        assert_eq!(lanes[3].0, 1);
    }
}

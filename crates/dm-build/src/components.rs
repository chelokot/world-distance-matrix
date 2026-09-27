use dm_core::weight::Weight;

pub struct Components {
    pub component: Vec<u32>,
    pub sizes: Vec<u32>,
}

impl Components {
    pub fn is_major(&self, node: u32, min_nodes: u32) -> bool {
        self.sizes[self.component[node as usize] as usize] >= min_nodes
    }
}

fn forward_adjacency(node_count: usize, arcs: impl Iterator<Item = (u32, u32, Weight)> + Clone) -> (Vec<u32>, Vec<u32>) {
    let mut first = vec![0u32; node_count + 1];
    for (tail, _, _) in arcs.clone() {
        first[tail as usize + 1] += 1;
    }
    for node in 0..node_count {
        first[node + 1] += first[node];
    }
    let mut cursor = first.clone();
    let mut heads = vec![0u32; first[node_count] as usize];
    for (tail, head, _) in arcs {
        heads[cursor[tail as usize] as usize] = head;
        cursor[tail as usize] += 1;
    }
    (first, heads)
}

const UNVISITED: u32 = u32::MAX;

struct Tarjan {
    first: Vec<u32>,
    heads: Vec<u32>,
    index: Vec<u32>,
    lowlink: Vec<u32>,
    on_stack: Vec<bool>,
    component: Vec<u32>,
    sizes: Vec<u32>,
    stack: Vec<u32>,
    calls: Vec<(u32, u32)>,
    counter: u32,
}

impl Tarjan {
    fn discover(&mut self, node: u32) {
        self.index[node as usize] = self.counter;
        self.lowlink[node as usize] = self.counter;
        self.counter += 1;
        self.stack.push(node);
        self.on_stack[node as usize] = true;
        self.calls.push((node, self.first[node as usize]));
    }

    fn finish(&mut self, node: u32) {
        let id = self.sizes.len() as u32;
        let mut size = 0u32;
        loop {
            let member = self.stack.pop().expect("component root is on the stack");
            self.on_stack[member as usize] = false;
            self.component[member as usize] = id;
            size += 1;
            if member == node {
                break;
            }
        }
        self.sizes.push(size);
    }

    fn visit(&mut self, root: u32) {
        self.discover(root);
        while let Some(&(node, position)) = self.calls.last() {
            if position < self.first[node as usize + 1] {
                self.calls.last_mut().expect("non-empty").1 += 1;
                let next = self.heads[position as usize];
                if self.index[next as usize] == UNVISITED {
                    self.discover(next);
                } else if self.on_stack[next as usize] {
                    self.lowlink[node as usize] = self.lowlink[node as usize].min(self.index[next as usize]);
                }
                continue;
            }
            self.calls.pop();
            if let Some(&(parent, _)) = self.calls.last() {
                self.lowlink[parent as usize] = self.lowlink[parent as usize].min(self.lowlink[node as usize]);
            }
            if self.lowlink[node as usize] == self.index[node as usize] {
                self.finish(node);
            }
        }
    }
}

pub fn strongly_connected(node_count: usize, arcs: impl Iterator<Item = (u32, u32, Weight)> + Clone) -> Components {
    let (first, heads) = forward_adjacency(node_count, arcs);
    let mut tarjan = Tarjan {
        first,
        heads,
        index: vec![UNVISITED; node_count],
        lowlink: vec![0; node_count],
        on_stack: vec![false; node_count],
        component: vec![UNVISITED; node_count],
        sizes: Vec::new(),
        stack: Vec::new(),
        calls: Vec::new(),
        counter: 0,
    };
    for root in 0..node_count as u32 {
        if tarjan.index[root as usize] == UNVISITED {
            tarjan.visit(root);
        }
    }
    Components { component: tarjan.component, sizes: tarjan.sizes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Chain;
    use dm_core::weight::{ChainCost, NOT_TRAVERSABLE};

    fn chain(tail: u32, head: u32, two_way: bool) -> Chain {
        Chain { tail, head, cost: ChainCost { dist_dm: 10, forward_time_ms: 10, backward_time_ms: if two_way { 10 } else { NOT_TRAVERSABLE } } }
    }

    #[test]
    fn finds_sinks_and_cycles() {
        let chains = [chain(0, 1, true), chain(1, 2, false), chain(2, 0, false), chain(2, 3, false), chain(4, 5, true)];
        let arcs = chains.iter().flat_map(|c| {
            let forward = c.cost.forward().map(|w| (c.tail, c.head, w));
            let backward = c.cost.backward().map(|w| (c.head, c.tail, w));
            forward.into_iter().chain(backward)
        });
        let components = strongly_connected(6, arcs);
        let c = &components.component;
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_ne!(c[2], c[3]);
        assert_eq!(c[4], c[5]);
        assert_ne!(c[0], c[4]);
        assert!(components.is_major(0, 3) && !components.is_major(3, 2) && !components.is_major(4, 3));
    }
}

use std::borrow::Cow;
use std::collections::{hash_map::Entry, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use crate::graph::{Graph, NodeId};
use crate::ir::{AssignKind, BinaryOp, Constant, ConstantValue, Statement, Value, VariableId};

pub type Location = usize;

pub type FastHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

#[derive(Default)]
pub struct FastHasher(u64);

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.write_u64(u64::from(value));
    }

    fn write_u16(&mut self, value: u16) {
        self.write_u64(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.write_u64(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517cc1b727220a95);
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn write_i8(&mut self, value: i8) {
        self.write_u64(value as u64);
    }

    fn write_i16(&mut self, value: i16) {
        self.write_u64(value as u64);
    }

    fn write_i32(&mut self, value: i32) {
        self.write_u64(value as u64);
    }

    fn write_i64(&mut self, value: i64) {
        self.write_u64(value as u64);
    }

    fn write_isize(&mut self, value: isize) {
        self.write_u64(value as u64);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Definition {
    Parameter(VariableId),
    Instruction(Location),
}

#[derive(Debug, Default)]
pub struct DefUseChains {
    pub use_def: FastHashMap<(VariableId, Location), Vec<Definition>>,
    pub def_use: FastHashMap<(VariableId, Definition), Vec<Location>>,
}

impl DefUseChains {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            use_def: FastHashMap::with_capacity_and_hasher(capacity, Default::default()),
            def_use: FastHashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }
}

#[derive(Clone, Copy)]
struct StatementLocation {
    node: NodeId,
    index: usize,
}

struct LocationIndex {
    statements: Vec<StatementLocation>,
    by_node: Vec<Vec<Location>>,
    has_instruction_definitions: bool,
}

struct ActiveLocations {
    by_node: Vec<Vec<Location>>,
    active: Vec<bool>,
    count: usize,
}

impl LocationIndex {
    fn build(graph: &Graph) -> Self {
        let mut statements = Vec::new();
        let mut by_node = vec![Vec::new(); graph.nodes.len()];
        let mut has_instruction_definitions = false;
        for &node in &graph.rpo {
            for index in 0..graph.nodes[node].statements.len() {
                by_node[node].push(statements.len());
                statements.push(StatementLocation { node, index });
                has_instruction_definitions |= graph.nodes[node].statements[index]
                    .defined_variable()
                    .is_some();
            }
        }
        Self {
            statements,
            by_node,
            has_instruction_definitions,
        }
    }

    fn build_with_definition_sites(graph: &Graph) -> (Self, Vec<(Location, usize)>) {
        let mut statements = Vec::new();
        let mut by_node = vec![Vec::new(); graph.nodes.len()];
        let mut definition_sites = Vec::new();
        for &node in &graph.rpo {
            for index in 0..graph.nodes[node].statements.len() {
                let location = statements.len();
                by_node[node].push(location);
                statements.push(StatementLocation { node, index });
                if let Some(variable) = graph.nodes[node].statements[index].defined_variable() {
                    let variable_index = graph
                        .variables
                        .dense_index(variable)
                        .expect("defined variable is registered");
                    definition_sites.push((location, variable_index));
                }
            }
        }
        (
            Self {
                statements,
                by_node,
                has_instruction_definitions: !definition_sites.is_empty(),
            },
            definition_sites,
        )
    }

    fn get(&self, location: Location) -> StatementLocation {
        self.statements[location]
    }

    fn node(&self, location: Location) -> NodeId {
        self.get(location).node
    }

    fn node_locations(&self, node: NodeId) -> &[Location] {
        &self.by_node[node]
    }
}

impl ActiveLocations {
    fn new(locations: &LocationIndex) -> Self {
        Self {
            by_node: locations.by_node.clone(),
            active: vec![true; locations.statements.len()],
            count: locations.statements.len(),
        }
    }

    fn total_count(&self) -> usize {
        self.count
    }

    fn count(&self, node: NodeId) -> usize {
        self.by_node[node].len()
    }

    fn is_removed(&self, location: Location) -> bool {
        !self.active[location]
    }

    fn select(&self, _locations: &LocationIndex, node: NodeId, rank: usize) -> Location {
        self.by_node[node][rank]
    }

    fn remove(&mut self, locations: &LocationIndex, location: Location) {
        if self.is_removed(location) {
            return;
        }
        self.active[location] = false;
        self.count -= 1;
        let statement = locations.get(location);
        let index = self.by_node[statement.node]
            .binary_search(&location)
            .expect("active location is present in its node");
        self.by_node[statement.node].remove(index);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ReachingDefinitions {
    bits: Vec<u64>,
}

struct DefinitionUniverse {
    definitions: Vec<Definition>,
    offsets: Vec<usize>,
    location_bits: Vec<usize>,
    blocked_words: Vec<Vec<(usize, u64)>>,
    generated_words: Vec<Vec<(usize, u64)>>,
}

impl DefinitionUniverse {
    fn build(
        graph: &Graph,
        locations: &LocationIndex,
        definition_sites: &[(Location, usize)],
    ) -> Self {
        let variable_count = graph.variables.dense_len();
        let parameters = graph.variables.parameter_ids();
        let mut counts = vec![0_usize; variable_count];
        for &parameter in &parameters {
            let index = graph.variables.dense_index(parameter).expect("known parameter");
            counts[index] += 1;
        }
        for &(_, index) in definition_sites {
            counts[index] += 1;
        }

        let mut offsets = Vec::with_capacity(variable_count + 1);
        offsets.push(0);
        for &count in &counts {
            offsets.push(offsets.last().copied().unwrap_or(0) + count);
        }
        let total = offsets.last().copied().unwrap_or(0);
        let mut definitions = vec![Definition::Instruction(usize::MAX); total];
        for (cursor, &offset) in counts.iter_mut().zip(&offsets) {
            *cursor = offset;
        }
        for parameter in parameters {
            let index = graph.variables.dense_index(parameter).expect("known parameter");
            definitions[counts[index]] = Definition::Parameter(parameter);
            counts[index] += 1;
        }

        let mut location_bits = vec![usize::MAX; locations.statements.len()];
        let mut blocked_words = vec![Vec::new(); graph.nodes.len()];
        let mut generated_words = vec![Vec::new(); graph.nodes.len()];
        let mut last_definition = vec![usize::MAX; variable_count];
        let mut node_variables = Vec::new();
        let mut site_cursor = 0;
        for &node in &graph.rpo {
            node_variables.clear();
            let node_locations = locations.node_locations(node);
            let node_end = node_locations.last().map_or(0, |location| location + 1);
            while definition_sites
                .get(site_cursor)
                .is_some_and(|(location, _)| *location < node_end)
            {
                let (location, index) = definition_sites[site_cursor];
                site_cursor += 1;
                let bit = counts[index];
                counts[index] += 1;
                definitions[bit] = Definition::Instruction(location);
                location_bits[location] = bit;
                if last_definition[index] == usize::MAX {
                    node_variables.push(index);
                }
                last_definition[index] = bit;
            }
            for &index in &node_variables {
                append_word_range_mask(
                    &mut blocked_words[node],
                    offsets[index]..offsets[index + 1],
                );
                let bit = last_definition[index];
                generated_words[node].push((bit / 64, 1_u64 << (bit % 64)));
                last_definition[index] = usize::MAX;
            }
            merge_word_masks(&mut blocked_words[node]);
            merge_word_masks(&mut generated_words[node]);
        }
        Self {
            definitions,
            offsets,
            location_bits,
            blocked_words,
            generated_words,
        }
    }

    fn range(&self, graph: &Graph, variable: VariableId) -> std::ops::Range<usize> {
        let index = graph.variables.dense_index(variable).expect("known variable");
        self.offsets[index]..self.offsets[index + 1]
    }

    fn bit(&self, graph: &Graph, variable: VariableId, definition: Definition) -> usize {
        match definition {
            Definition::Parameter(_) => self.range(graph, variable).start,
            Definition::Instruction(location) => self.location_bits[location],
        }
    }
}

fn append_word_range_mask(words: &mut Vec<(usize, u64)>, range: std::ops::Range<usize>) {
    if range.is_empty() {
        return;
    }
    let first_word = range.start / 64;
    let last_word = (range.end - 1) / 64;
    let first_mask = u64::MAX << (range.start % 64);
    let last_mask = u64::MAX >> (63 - ((range.end - 1) % 64));
    if first_word == last_word {
        words.push((first_word, first_mask & last_mask));
        return;
    }
    words.push((first_word, first_mask));
    for word in first_word + 1..last_word {
        words.push((word, u64::MAX));
    }
    words.push((last_word, last_mask));
}

fn merge_word_masks(words: &mut Vec<(usize, u64)>) {
    words.sort_unstable_by_key(|(word, _)| *word);
    let mut output = 0;
    for input in 0..words.len() {
        if output != 0 && words[output - 1].0 == words[input].0 {
            words[output - 1].1 |= words[input].1;
        } else {
            words[output] = words[input];
            output += 1;
        }
    }
    words.truncate(output);
}

fn clear_bit_range(bits: &mut [u64], range: std::ops::Range<usize>) {
    if range.is_empty() {
        return;
    }
    let first_word = range.start / 64;
    let last_word = (range.end - 1) / 64;
    let first_mask = u64::MAX << (range.start % 64);
    let last_mask = u64::MAX >> (63 - ((range.end - 1) % 64));
    if first_word == last_word {
        bits[first_word] &= !(first_mask & last_mask);
        return;
    }
    bits[first_word] &= !first_mask;
    bits[last_word] &= !last_mask;
    bits[first_word + 1..last_word].fill(0);
}

impl ReachingDefinitions {
    fn empty(universe: &DefinitionUniverse) -> Self {
        Self {
            bits: vec![0; universe.definitions.len().div_ceil(64)],
        }
    }

    fn definitions(
        &self,
        graph: &Graph,
        universe: &DefinitionUniverse,
        variable: VariableId,
    ) -> Vec<Definition> {
        let range = universe.range(graph, variable);
        let mut definitions = Vec::with_capacity(1);
        for bit in range {
            if self.bits[bit / 64] & (1_u64 << (bit % 64)) != 0 {
                definitions.push(universe.definitions[bit]);
            }
        }
        definitions
    }

    fn insert(
        &mut self,
        graph: &Graph,
        universe: &DefinitionUniverse,
        variable: VariableId,
        definition: Definition,
    ) {
        let bit = universe.bit(graph, variable, definition);
        self.bits[bit / 64] |= 1_u64 << (bit % 64);
    }

    fn replace(
        &mut self,
        graph: &Graph,
        universe: &DefinitionUniverse,
        variable: VariableId,
        definition: Definition,
    ) {
        clear_bit_range(&mut self.bits, universe.range(graph, variable));
        self.insert(graph, universe, variable, definition);
    }

    fn union_transfer_from(
        &mut self,
        other: &Self,
        universe: &DefinitionUniverse,
        node: NodeId,
    ) -> bool {
        let blocked = &universe.blocked_words[node];
        let generated = &universe.generated_words[node];
        let mut blocked_index = 0;
        let mut generated_index = 0;
        let mut changed = false;
        for (word, (current, source)) in self.bits.iter_mut().zip(&other.bits).enumerate() {
            let blocked_mask = if blocked
                .get(blocked_index)
                .is_some_and(|(blocked_word, _)| *blocked_word == word)
            {
                let mask = blocked[blocked_index].1;
                blocked_index += 1;
                mask
            } else {
                0
            };
            let generated_mask = if generated
                .get(generated_index)
                .is_some_and(|(generated_word, _)| *generated_word == word)
            {
                let mask = generated[generated_index].1;
                generated_index += 1;
                mask
            } else {
                0
            };
            let merged = *current | (*source & !blocked_mask) | generated_mask;
            changed |= merged != *current;
            *current = merged;
        }
        changed
    }

    fn union_generated(&mut self, universe: &DefinitionUniverse, node: NodeId) -> bool {
        let mut changed = false;
        for &(word, mask) in &universe.generated_words[node] {
            let merged = self.bits[word] | mask;
            changed |= merged != self.bits[word];
            self.bits[word] = merged;
        }
        changed
    }
}

fn union_predecessor_transfer(
    states: &mut [ReachingDefinitions],
    node: NodeId,
    predecessor: NodeId,
    universe: &DefinitionUniverse,
) -> bool {
    if node == predecessor {
        return states[node].union_generated(universe, predecessor);
    }
    if predecessor < node {
        let (before_node, node_and_after) = states.split_at_mut(node);
        node_and_after[0].union_transfer_from(&before_node[predecessor], universe, predecessor)
    } else {
        let (before_predecessor, predecessor_and_after) = states.split_at_mut(predecessor);
        before_predecessor[node].union_transfer_from(
            &predecessor_and_after[0],
            universe,
            predecessor,
        )
    }
}

pub fn optimize(graph: &mut Graph) -> DefUseChains {
    optimize_impl::<true, false>(graph, "V")
}

pub(crate) fn optimize_for_decompilation(graph: &mut Graph) {
    drop(optimize_impl::<false, false>(graph, "V"));
}

pub(crate) fn optimize_for_decompilation_enhanced(graph: &mut Graph, return_type: &str) {
    drop(optimize_impl::<false, true>(graph, return_type));
}

fn optimize_impl<const PRESERVE_CHAINS: bool, const ENHANCED: bool>(
    graph: &mut Graph,
    return_type: &str,
) -> DefUseChains {
    let (locations, definition_sites) = LocationIndex::build_with_definition_sites(graph);
    let mut chains = build_def_use_at(graph, &locations, &definition_sites);
    if !locations.has_instruction_definitions {
        if ENHANCED {
            recover_types_and_constants(graph, return_type);
        }
        return chains;
    }
    split_variables_at::<ENHANCED>(graph, &mut chains, &locations);
    let mut live_locations = ActiveLocations::new(&locations);
    dead_code_elimination(graph, &locations, &mut live_locations, &mut chains);
    if chains.use_def.is_empty() && chains.def_use.is_empty() {
        compact_removed_without_index(graph, &locations, &live_locations);
        if ENHANCED {
            recover_types_and_constants(graph, return_type);
        }
        return chains;
    }
    if !chains_have_instruction_definitions(&chains) {
        remap_chain_locations(&mut chains, &locations, &live_locations);
        compact_removed_without_index(graph, &locations, &live_locations);
        if ENHANCED {
            recover_types_and_constants(graph, return_type);
        }
        return chains;
    }
    register_propagation::<ENHANCED>(
        graph,
        &locations,
        &mut live_locations,
        &mut chains,
    );
    if PRESERVE_CHAINS {
        remap_chain_locations(&mut chains, &locations, &live_locations);
        let compacted_locations = compact_removed(graph, &locations, &live_locations);
        place_declarations_at::<PRESERVE_CHAINS, ENHANCED>(
            graph,
            &chains,
            &compacted_locations,
            None,
        );
    } else {
        place_declarations_at::<PRESERVE_CHAINS, ENHANCED>(
            graph,
            &chains,
            &locations,
            Some(&live_locations),
        );
        compact_removed_without_index(graph, &locations, &live_locations);
    }
    if ENHANCED {
        recover_types_and_constants(graph, return_type);
    }
    chains
}

pub fn build_def_use(graph: &Graph) -> DefUseChains {
    let (locations, definition_sites) = LocationIndex::build_with_definition_sites(graph);
    build_def_use_at(graph, &locations, &definition_sites)
}

fn collect_unique_used_variables(statement: &Statement, output: &mut Vec<VariableId>) {
    output.clear();
    statement.collect_used_variables_vec(output);
    if output.len() > 1 {
        output.sort_unstable();
        output.dedup();
    }
}

fn chains_have_instruction_definitions(chains: &DefUseChains) -> bool {
    let in_def_use = chains
        .def_use
        .keys()
        .any(|(_, definition)| matches!(definition, Definition::Instruction(_)));
    debug_assert_eq!(
        in_def_use,
        chains
            .use_def
            .values()
            .flatten()
            .any(|definition| matches!(definition, Definition::Instruction(_)))
    );
    in_def_use
}

fn build_def_use_at(
    graph: &Graph,
    locations: &LocationIndex,
    definition_sites: &[(Location, usize)],
) -> DefUseChains {
    if let Some(chains) = build_linear_def_use(graph, locations) {
        return chains;
    }

    let universe = DefinitionUniverse::build(graph, locations, definition_sites);
    let mut incoming = vec![ReachingDefinitions::empty(&universe); graph.nodes.len()];
    for parameter in graph.variables.parameter_ids() {
        incoming[graph.entry].insert(
            graph,
            &universe,
            parameter,
            Definition::Parameter(parameter),
        );
    }

    let mut queue = VecDeque::from(graph.rpo.clone());
    let mut queued = vec![false; graph.nodes.len()];
    let mut processed = vec![false; graph.nodes.len()];
    for &node in &graph.rpo {
        queued[node] = true;
    }
    while let Some(node) = queue.pop_front() {
        queued[node] = false;
        let first_visit = !processed[node];
        let mut incoming_changed = first_visit;
        for predecessor in graph.all_predecessors(node) {
            incoming_changed |=
                union_predecessor_transfer(&mut incoming, node, predecessor, &universe);
        }
        if !incoming_changed {
            continue;
        }
        processed[node] = true;

        for successor in graph.all_successors(node) {
            if !queued[successor] {
                queued[successor] = true;
                queue.push_back(successor);
            }
        }
    }

    let mut chains = DefUseChains::with_capacity(locations.statements.len());
    let mut used_variables = Vec::new();
    for &node in &graph.rpo {
        let reaching = &mut incoming[node];
        for &location in locations.node_locations(node) {
            let statement_location = locations.get(location);
            let statement = &graph.nodes[node].statements[statement_location.index];
            used_variables.clear();
            statement.collect_unique_used_variables_vec(&mut used_variables);
            for &variable in &used_variables {
                let mut definitions = reaching.definitions(graph, &universe, variable);
                if definitions.is_empty() {
                    continue;
                }
                definitions.sort_by_key(definition_sort_key);
                for &definition in &definitions {
                    chains
                        .def_use
                        .entry((variable, definition))
                        .or_default()
                        .push(location);
                }
                chains
                    .use_def
                    .insert((variable, location), definitions);
            }
            if let Some(variable) = statement.defined_variable() {
                reaching.replace(
                    graph,
                    &universe,
                    variable,
                    Definition::Instruction(location),
                );
            }
        }
    }
    chains
}

fn build_linear_def_use(graph: &Graph, locations: &LocationIndex) -> Option<DefUseChains> {
    let &[node] = graph.rpo.as_slice() else {
        return None;
    };
    if graph.all_predecessors(node).next().is_some()
        || graph.all_successors(node).next().is_some()
    {
        return None;
    }

    let mut chains = DefUseChains::with_capacity(locations.statements.len());
    let mut definitions = FastHashMap::<VariableId, Definition>::default();
    for parameter in graph.variables.parameter_ids() {
        definitions.insert(parameter, Definition::Parameter(parameter));
    }

    let mut used_variables = Vec::new();
    for &location in locations.node_locations(node) {
        let statement_location = locations.get(location);
        let statement = &graph.nodes[statement_location.node].statements[statement_location.index];
        used_variables.clear();
        statement.collect_unique_used_variables_vec(&mut used_variables);
        for &variable in &used_variables {
            let Some(&definition) = definitions.get(&variable) else {
                continue;
            };
            chains
                .def_use
                .entry((variable, definition))
                .or_default()
                .push(location);
            chains
                .use_def
                .insert((variable, location), vec![definition]);
        }
        if let Some(variable) = statement.defined_variable() {
            definitions.insert(variable, Definition::Instruction(location));
        }
    }
    Some(chains)
}

pub fn split_variables(graph: &mut Graph, chains: &mut DefUseChains) {
    let locations = LocationIndex::build(graph);
    split_variables_at::<false>(graph, chains, &locations);
}

fn split_variables_at<const ENHANCED: bool>(
    graph: &mut Graph,
    chains: &mut DefUseChains,
    locations: &LocationIndex,
) {
    let mut definitions_by_variable = FastHashMap::<VariableId, Vec<Definition>>::default();
    for &(variable, definition) in chains.def_use.keys() {
        definitions_by_variable
            .entry(variable)
            .or_default()
            .push(definition);
    }
    if ENHANCED {
        for parameter in graph.variables.parameter_ids() {
            if let Some(defs) = definitions_by_variable.get_mut(&parameter)
                && !defs.contains(&Definition::Parameter(parameter))
            {
                defs.push(Definition::Parameter(parameter));
            }
        }
    }
    let allow_integer_type_fix = graph.nodes.iter().any(|node| {
        let has_string_char_array = node.statements.iter().any(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    rhs: Value::Invoke(invoke),
                    ..
                } if invoke.method.return_type == "[C"
            )
        });
        let has_array_length = node
            .statements
            .iter()
            .any(|statement| matches!(statement, Statement::Assign {
                rhs: Value::ArrayLength(_),
                ..
            }));
        has_string_char_array && has_array_length
    });

    let mut variables: Vec<_> = definitions_by_variable.into_iter().collect();
    variables.sort_by_key(|(variable, _)| *variable);
    let mut definition_versions = FastHashMap::default();
    let mut use_marks = vec![false; locations.statements.len()];
    for (variable, mut definitions) in variables {
        definitions.sort_by_key(definition_order_key);
        definitions.dedup();
        let ordered_definitions = definitions.clone();
        let has_parameter_def = ENHANCED
            && ordered_definitions.first() == Some(&Definition::Parameter(variable))
            && ordered_definitions.len() > 1;
        let mut remaining = vec![true; ordered_definitions.len()];
        let mut groups: Vec<(Vec<Definition>, Vec<Location>)> = Vec::new();
        let mut definition_queue = VecDeque::new();
        while let Some((_, start)) = ordered_definitions
            .iter()
            .copied()
            .enumerate()
            .find(|(index, _)| remaining[*index])
        {
            let mut group_definitions = Vec::new();
            let mut group_uses = Vec::new();
            definition_queue.clear();
            definition_queue.push_back(start);
            while let Some(definition) = definition_queue.pop_front() {
                let definition_index = ordered_definitions
                    .binary_search_by(|candidate| {
                        definition_order_key(candidate).cmp(&definition_order_key(&definition))
                    })
                    .expect("definition is present in ordered definitions");
                if !remaining[definition_index] {
                    continue;
                }
                remaining[definition_index] = false;
                group_definitions.push(definition);
                if let Some(uses) = chains.def_use.get(&(variable, definition)).cloned() {
                    for usage in uses {
                        let skip_catch_spillover = has_parameter_def
                            && matches!(definition, Definition::Instruction(def_loc) if !graph.nodes[locations.node(def_loc)].in_catch && graph.nodes[locations.node(def_loc)].catch_successors.contains(&locations.node(usage)))
                            && graph.nodes[locations.node(usage)].in_catch
                            && chains
                                .use_def
                                .get(&(variable, usage))
                                .is_some_and(|reaching| {
                                    reaching.iter().all(|cand| match cand {
                                        Definition::Parameter(_) => true,
                                        Definition::Instruction(loc) => {
                                            let def_node = locations.node(*loc);
                                            !graph.nodes[def_node].in_catch
                                                && graph.nodes[def_node]
                                                    .catch_successors
                                                    .contains(&locations.node(usage))
                                        }
                                    })
                                });
                        if skip_catch_spillover {
                            if let Some((param_defs, param_uses)) = groups.first_mut()
                                && param_defs.contains(&Definition::Parameter(variable))
                                && !param_uses.contains(&usage)
                            {
                                param_uses.push(usage);
                                chains
                                    .def_use
                                    .entry((variable, Definition::Parameter(variable)))
                                    .or_default()
                                    .push(usage);
                                if let Some(reaching) = chains.use_def.get_mut(&(variable, usage))
                                    && !reaching.contains(&Definition::Parameter(variable))
                                {
                                    reaching.push(Definition::Parameter(variable));
                                }
                            }
                            continue;
                        }
                        if !use_marks[usage] {
                            use_marks[usage] = true;
                            group_uses.push(usage);
                            if let Some(reaching) = chains.use_def.get(&(variable, usage)) {
                                for &candidate in reaching {
                                    let skip_reverse = has_parameter_def
                                        && matches!(definition, Definition::Parameter(_))
                                        && graph.nodes[locations.node(usage)].in_catch
                                        && matches!(candidate, Definition::Instruction(def_loc) if !graph.nodes[locations.node(def_loc)].in_catch && graph.nodes[locations.node(def_loc)].catch_successors.contains(&locations.node(usage)));
                                    if !skip_reverse {
                                        definition_queue.push_back(candidate);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for &usage in &group_uses {
                use_marks[usage] = false;
            }
            groups.push((group_definitions, group_uses));
        }
        if groups.len() <= 1 {
            continue;
        }

        let original_metadata = graph
            .variables
            .cloned_metadata(variable)
            .expect("def-use references a known variable");
        for (version, (definitions, uses)) in groups.into_iter().enumerate() {
            let replacement = if definitions.contains(&Definition::Parameter(variable)) {
                if !ENHANCED {
                    graph
                        .variables
                        .set_versioned_name(variable, &original_metadata, version);
                } else {
                    for &location in &uses {
                        if let Some(reaching) = chains.use_def.get_mut(&(variable, location)) {
                            reaching.retain(|def| definitions.contains(def));
                        }
                        for &other_def in &ordered_definitions {
                            if !definitions.contains(&other_def)
                                && let Some(def_uses) = chains.def_use.get_mut(&(variable, other_def))
                            {
                                def_uses.retain(|u| *u != location);
                            }
                        }
                    }
                }
                variable
            } else {
                graph
                    .variables
                    .version_from_metadata(variable, version, &original_metadata)
            };
            for &definition in &definitions {
                definition_versions.insert((variable, definition), replacement);
            }
            if replacement != variable {
                if let Some(ty) = infer_definition_group_type::<ENHANCED>(graph, locations, &definitions) {
                    let original_type = graph
                        .variables
                        .get(replacement)
                        .and_then(|info| info.ty.as_deref());
                    if (ENHANCED && ty.as_ref() != "unknownType")
                        || (original_type == Some("Ljava/lang/String;")
                            && (ty.as_ref() == "[C"
                                || (ty.as_ref() == "I" && allow_integer_type_fix)))
                    {
                        graph
                            .variables
                            .get_mut(replacement)
                            .expect("created version variable")
                            .ty = Some(ty);
                    }
                } else if ENHANCED {
                    graph
                        .variables
                        .get_mut(replacement)
                        .expect("created version variable")
                        .ty = None;
                }
                for location in uses {
                    let statement = locations.get(location);
                    let mut remap = |candidate| {
                        if candidate == variable {
                            replacement
                        } else {
                            candidate
                        }
                    };
                    graph.nodes[statement.node].statements[statement.index]
                        .remap_used_variables(&mut remap);
                    let mut reaching = chains
                        .use_def
                        .remove(&(variable, location))
                        .expect("split use has a use-def chain");
                    if ENHANCED {
                        reaching.retain(|def| definitions.contains(def));
                        for &other_def in &ordered_definitions {
                            if !definitions.contains(&other_def)
                                && let Some(def_uses) = chains.def_use.get_mut(&(variable, other_def))
                            {
                                def_uses.retain(|u| *u != location);
                            }
                        }
                    }
                    let previous = chains
                        .use_def
                        .insert((replacement, location), reaching);
                    debug_assert!(previous.is_none());
                }
            }
            for definition in definitions {
                let Definition::Instruction(location) = definition else {
                    continue;
                };
                let statement = locations.get(location);
                graph.nodes[statement.node].statements[statement.index]
                    .replace_defined_variable(replacement);
            }
        }
    }
    remap_definition_chain_variables(chains, &definition_versions);
}

fn infer_definition_group_type<const ENHANCED: bool>(
    graph: &Graph,
    locations: &LocationIndex,
    definitions: &[Definition],
) -> Option<Cow<'static, str>> {
    let mut inferred: Option<(Cow<'static, str>, bool)> = None;
    for definition in definitions {
        let Definition::Instruction(location) = definition else {
            continue;
        };
        let statement_location = locations.get(*location);
        let statement =
            &graph.nodes[statement_location.node].statements[statement_location.index];
        let Some((ty, is_zero_int)) = (match statement {
            Statement::Assign { rhs, .. } => {
                let is_zero = matches!(
                    rhs,
                    Value::Constant(Constant {
                        value: ConstantValue::Integer { value: 0, ty: "I" },
                    })
                );
                let t = if let Value::Invoke(invoke) = rhs
                    && invoke.method.name == "<init>"
                    && invoke.method.return_type == "V"
                {
                    value_type(&invoke.base, graph)
                } else {
                    value_type_for_split::<ENHANCED>(rhs, graph)
                };
                if is_zero && ENHANCED {
                    None
                } else {
                    t.map(|t| (t, is_zero))
                }
            }
            Statement::MoveException { ty, .. } => ty
                .as_deref()
                .map(str::to_owned)
                .map(Cow::Owned)
                .map(|t| (t, false)),
            _ => None,
        }) else {
            continue;
        };
        if let Some((existing, existing_is_zero)) = inferred.as_ref() {
            if existing.as_ref() == ty.as_ref() {
                inferred = Some((ty, *existing_is_zero && is_zero_int));
                continue;
            }
            if ENHANCED {
                if *existing_is_zero
                    && existing.as_ref() == "I"
                    && (ty.starts_with('L') || ty.starts_with('[') || ty.as_ref() == "Z")
                {
                    inferred = Some((ty, false));
                    continue;
                }
                if is_zero_int
                    && ty.as_ref() == "I"
                    && (existing.starts_with('L') || existing.starts_with('[') || existing.as_ref() == "Z")
                {
                    continue;
                }
            }
            return None;
        }
        inferred = Some((ty, is_zero_int));
    }
    inferred.map(|(ty, _)| ty)
}

fn value_type_for_split<const ENHANCED: bool>(
    value: &Value,
    graph: &Graph,
) -> Option<Cow<'static, str>> {
    match value {
        Value::Variable(variable) if ENHANCED && !graph.variables.is_declared(*variable) => None,
        _ => value_type(value, graph),
    }
}

fn value_type(value: &Value, graph: &Graph) -> Option<Cow<'static, str>> {
    match value {
        Value::Variable(variable) => graph.variables.get(*variable).and_then(|info| info.ty.clone()),
        Value::Constant(constant) => Some(Cow::Borrowed(constant.ty())),
        Value::NewInstance { ty } => Some(Cow::Owned(ty.clone())),
        Value::Invoke(invoke) => Some(Cow::Owned(invoke.method.return_type.clone())),
        Value::CheckCast(cast) => Some(Cow::Owned(cast.descriptor.clone())),
        Value::ArrayLength(_) => Some(Cow::Borrowed("I")),
        Value::NewArray(array) => Some(Cow::Owned(array.ty.clone())),
        Value::FilledArray(array) => Some(Cow::Owned(array.ty.clone())),
        Value::Binary(binary) => Some(Cow::Borrowed(binary.ty)),
        Value::Unary(unary) => Some(Cow::Borrowed(unary.ty)),
        Value::ArrayLoad(load) => {
            value_type(&load.array, graph)
                .and_then(|ty| ty.strip_prefix('[').map(|element| Cow::Owned(element.to_owned())))
                .or_else(|| load.ty.map(Cow::Borrowed))
        }
        Value::InstanceField(field) => Some(Cow::Owned(field.field.ty.to_string())),
        Value::StaticField(field) => Some(Cow::Owned(field.ty.to_string())),
        Value::BaseClass { .. } => Some(Cow::Borrowed("Ljava/lang/Class;")),
    }
}

fn remap_definition_chain_variables(
    chains: &mut DefUseChains,
    definition_versions: &FastHashMap<(VariableId, Definition), VariableId>,
) {
    let def_use = std::mem::take(&mut chains.def_use);
    for ((variable, definition), uses) in def_use {
        let variable = definition_versions
            .get(&(variable, definition))
            .copied()
            .unwrap_or(variable);
        chains.def_use.insert((variable, definition), uses);
    }
}

fn remap_chain_locations(
    chains: &mut DefUseChains,
    location_index: &LocationIndex,
    live_locations: &ActiveLocations,
) {
    let mut locations = vec![None; location_index.statements.len()];
    let mut next = 0;
    for (location, remapped) in locations.iter_mut().enumerate() {
        if !live_locations.is_removed(location) {
            *remapped = Some(next);
            next += 1;
        }
    }

    let use_def = std::mem::take(&mut chains.use_def);
    chains.use_def =
        FastHashMap::with_capacity_and_hasher(use_def.len(), Default::default());
    for ((variable, location), definitions) in use_def {
        let Some(location) = locations[location] else {
            continue;
        };
        let definitions: Vec<_> = definitions
            .into_iter()
            .filter_map(|definition| remap_definition_location(definition, &locations))
            .collect();
        if !definitions.is_empty() {
            chains.use_def.insert((variable, location), definitions);
        }
    }

    let def_use = std::mem::take(&mut chains.def_use);
    chains.def_use =
        FastHashMap::with_capacity_and_hasher(def_use.len(), Default::default());
    for ((variable, definition), uses) in def_use {
        let Some(definition) = remap_definition_location(definition, &locations) else {
            continue;
        };
        let uses: Vec<_> = uses
            .into_iter()
            .filter_map(|location| locations[location])
            .collect();
        if !uses.is_empty() {
            chains.def_use.insert((variable, definition), uses);
        }
    }
}

fn remap_definition_location(
    definition: Definition,
    locations: &[Option<Location>],
) -> Option<Definition> {
    match definition {
        Definition::Parameter(variable) => Some(Definition::Parameter(variable)),
        Definition::Instruction(location) => locations[location].map(Definition::Instruction),
    }
}

fn definition_order_key(definition: &Definition) -> (u8, usize) {
    match definition {
        Definition::Parameter(_) => (0, 0),
        Definition::Instruction(location) => (1, *location),
    }
}

fn dead_code_elimination(
    graph: &mut Graph,
    locations: &LocationIndex,
    live_locations: &mut ActiveLocations,
    chains: &mut DefUseChains,
) {
    for rpo_index in 0..graph.rpo.len() {
        let node = graph.rpo[rpo_index];
        let mut cursor = 0;
        while cursor < live_locations.count(node) {
            let location = live_locations.select(locations, node, cursor);
            let statement_location = locations.get(location);
            let statement =
                &graph.nodes[statement_location.node].statements[statement_location.index];
            let Some(variable) = statement.defined_variable() else {
                cursor += 1;
                continue;
            };
            if chains
                .def_use
                .contains_key(&(variable, Definition::Instruction(location)))
            {
                cursor += 1;
                continue;
            }
            if statement.is_call() {
                graph.nodes[statement_location.node].statements[statement_location.index]
                    .remove_defined_variable();
                cursor += 1;
            } else if statement.has_side_effect() {
                cursor += 1;
            } else {
                update_chain(graph, locations, live_locations, chains, location);
                remove_live_location(locations, live_locations, location);
                cursor += 1;
            }
        }
    }
}

fn update_chain(
    graph: &mut Graph,
    locations: &LocationIndex,
    live_locations: &mut ActiveLocations,
    chains: &mut DefUseChains,
    location: Location,
) {
    if live_locations.is_removed(location) {
        return;
    }
    let statement_location = locations.get(location);
    let mut used_variables = Vec::new();
    collect_unique_used_variables(
        &graph.nodes[statement_location.node].statements[statement_location.index],
        &mut used_variables,
    );
    for variable in used_variables {
        let definitions = chains
            .use_def
            .remove(&(variable, location))
            .unwrap_or_default();
        for definition in definitions {
            let key = (variable, definition);
            let direct_removal = chains
                .def_use
                .get(&key)
                .is_some_and(|uses| uses.as_slice() == [location]);
            let became_empty = if direct_removal {
                chains.def_use.remove(&key);
                true
            } else if let Some(uses) = chains.def_use.get_mut(&key) {
                uses.retain(|usage| *usage != location);
                uses.is_empty()
            } else {
                false
            };
            if !became_empty {
                continue;
            }
            if !direct_removal {
                chains.def_use.remove(&key);
            }
            let Definition::Instruction(definition_location) = definition else {
                continue;
            };
            if live_locations.is_removed(definition_location) {
                continue;
            }
            let origin_location = locations.get(definition_location);
            let origin = &graph.nodes[origin_location.node].statements[origin_location.index];
            if origin.is_call() {
                graph.nodes[origin_location.node].statements[origin_location.index]
                    .remove_defined_variable();
            } else if !origin.has_side_effect() {
                update_chain(
                    graph,
                    locations,
                    live_locations,
                    chains,
                    definition_location,
                );
                remove_live_location(locations, live_locations, definition_location);
            }
        }
    }
}

fn remove_live_location(
    locations: &LocationIndex,
    live_locations: &mut ActiveLocations,
    location: Location,
) {
    live_locations.remove(locations, location);
}

fn register_propagation<const ENHANCED: bool>(
    graph: &mut Graph,
    locations: &LocationIndex,
    live_locations: &mut ActiveLocations,
    chains: &mut DefUseChains,
) {
    let mut used_variables = Vec::new();
    let mut origin_variables = Vec::new();
    let mut current_variables = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for rpo_index in 0..graph.rpo.len() {
            let node = graph.rpo[rpo_index];
            let mut cursor = 0;
            while cursor < live_locations.count(node) {
                let use_location = live_locations.select(locations, node, cursor);
                let statement_location = locations.get(use_location);
                used_variables.clear();
                graph.nodes[statement_location.node].statements[statement_location.index]
                    .collect_used_variables_vec(&mut used_variables);
                if used_variables.len() > 1 {
                    used_variables.sort();
                    used_variables.dedup();
                }
                for variable in used_variables.iter().copied() {
                    let Some(definition_location) = chains
                        .use_def
                        .get(&(variable, use_location))
                        .and_then(|definitions| match definitions.as_slice() {
                            [Definition::Instruction(location)] => Some(*location),
                            _ => None,
                        })
                    else {
                        continue;
                    };
                    if live_locations.is_removed(definition_location) {
                        continue;
                    }
                    let definition_statement = locations.get(definition_location);
                    let origin = &graph.nodes[definition_statement.node].statements
                        [definition_statement.index];
                    let Some(rhs) = origin.rhs() else {
                        continue;
                    };
                    if !origin.is_propagable() {
                        continue;
                    }
                    origin_variables.clear();
                    origin.collect_used_variables_vec(&mut origin_variables);
                    if origin_variables.len() > 1 {
                        origin_variables.sort();
                        origin_variables.dedup();
                    }
                    let definition_key =
                        (variable, Definition::Instruction(definition_location));
                    let is_constant = rhs.is_constant(&graph.variables);
                    let is_compare_in_condition = ENHANCED
                        && matches!(rhs, Value::Binary(binary) if binary.op == BinaryOp::Compare && !rhs.has_side_effect())
                        && matches!(
                            graph.nodes[statement_location.node].statements
                                [statement_location.index],
                            Statement::Condition { .. }
                        );
                    let mut remove_single_use_definition = false;
                    if !is_constant {
                        if let Some(uses) = chains.def_use.get(&definition_key) {
                            if uses.len() > 1 && !is_compare_in_condition {
                                continue;
                            }
                            remove_single_use_definition =
                                matches!(uses.as_slice(), [usage] if *usage == use_location);
                        }
                        if origin_variables.iter().copied().any(|used| {
                            if is_compare_in_condition {
                                !clear_variable_path(
                                    graph,
                                    locations,
                                    live_locations,
                                    used,
                                    definition_location,
                                    use_location,
                                )
                            } else {
                                !clear_path(
                                    graph,
                                    locations,
                                    live_locations,
                                    Some(used),
                                    definition_location,
                                    use_location,
                                )
                            }
                        }) {
                            continue;
                        }
                    }
                    if origin.has_side_effect()
                        && !clear_path(
                            graph,
                            locations,
                            live_locations,
                            None,
                            definition_location,
                            use_location,
                        )
                    {
                        continue;
                    }

                    let move_rhs = if remove_single_use_definition {
                        current_variables.clear();
                        graph.nodes[statement_location.node].statements
                            [statement_location.index]
                            .collect_used_variables_vec(&mut current_variables);
                        current_variables
                            .iter()
                            .filter(|used| **used == variable)
                            .take(2)
                            .count()
                            == 1
                    } else {
                        false
                    };
                    let mut removed_definition_uses = if move_rhs {
                        chains.def_use.remove(&definition_key)
                    } else {
                        None
                    };
                    let moved_rhs = removed_definition_uses.is_some()
                        && move_definition_rhs(
                            graph,
                            definition_statement,
                            statement_location,
                            variable,
                        );
                    if !moved_rhs {
                        if let Some(uses) = removed_definition_uses.take() {
                            chains.def_use.insert(definition_key, uses);
                        }
                        replace_with_definition_rhs(
                            graph,
                            definition_statement,
                            statement_location,
                            variable,
                        );
                    }
                    chains.use_def.remove(&(variable, use_location));
                    let def_still_has_other_uses = !moved_rhs
                        && !remove_single_use_definition
                        && chains
                            .def_use
                            .get(&definition_key)
                            .is_some_and(|uses| uses.iter().any(|usage| *usage != use_location));
                    for origin_variable in origin_variables.iter().copied() {
                        let old_definitions = if def_still_has_other_uses {
                            chains
                                .use_def
                                .get(&(origin_variable, definition_location))
                                .cloned()
                        } else {
                            chains
                                .use_def
                                .remove(&(origin_variable, definition_location))
                        };
                        let Some(old_definitions) = old_definitions else {
                            continue;
                        };
                        for &old_definition in &old_definitions {
                            if let Some(uses) =
                                chains.def_use.get_mut(&(origin_variable, old_definition))
                            {
                                if !def_still_has_other_uses {
                                    uses.retain(|usage| *usage != definition_location);
                                }
                                if !uses.contains(&use_location) {
                                    uses.push(use_location);
                                }
                            }
                        }
                        match chains.use_def.entry((origin_variable, use_location)) {
                            Entry::Vacant(entry) => {
                                entry.insert(old_definitions);
                            }
                            Entry::Occupied(mut entry) => {
                                entry.get_mut().extend(old_definitions);
                            }
                        }
                    }
                    if moved_rhs {
                        if ENHANCED {
                            update_chain(
                                graph,
                                locations,
                                live_locations,
                                chains,
                                definition_location,
                            );
                        }
                        remove_live_location(
                            locations,
                            live_locations,
                            definition_location,
                        );
                        changed = true;
                    } else if remove_single_use_definition {
                        if chains.def_use.remove(&definition_key).is_some() {
                            if ENHANCED {
                                update_chain(
                                    graph,
                                    locations,
                                    live_locations,
                                    chains,
                                    definition_location,
                                );
                            }
                            remove_live_location(
                                locations,
                                live_locations,
                                definition_location,
                            );
                            changed = true;
                        }
                    } else if let Some(uses) = chains.def_use.get_mut(&definition_key) {
                        uses.retain(|usage| *usage != use_location);
                        if uses.is_empty() {
                            chains.def_use.remove(&definition_key);
                            if ENHANCED {
                                update_chain(
                                    graph,
                                    locations,
                                    live_locations,
                                    chains,
                                    definition_location,
                                );
                            }
                            remove_live_location(
                                locations,
                                live_locations,
                                definition_location,
                            );
                            changed = true;
                        }
                    }
                }
                cursor += 1;
            }
        }
    }
}

fn statement_pair_mut(
    graph: &mut Graph,
    first: StatementLocation,
    second: StatementLocation,
) -> (&mut Statement, &mut Statement) {
    assert_ne!((first.node, first.index), (second.node, second.index));
    if first.node == second.node {
        let statements = &mut graph.nodes[first.node].statements;
        if first.index < second.index {
            let (before_second, second_and_after) = statements.split_at_mut(second.index);
            (&mut before_second[first.index], &mut second_and_after[0])
        } else {
            let (before_first, first_and_after) = statements.split_at_mut(first.index);
            (&mut first_and_after[0], &mut before_first[second.index])
        }
    } else if first.node < second.node {
        let (before_second, second_and_after) = graph.nodes.split_at_mut(second.node);
        (
            &mut before_second[first.node].statements[first.index],
            &mut second_and_after[0].statements[second.index],
        )
    } else {
        let (before_first, first_and_after) = graph.nodes.split_at_mut(first.node);
        (
            &mut first_and_after[0].statements[first.index],
            &mut before_first[second.node].statements[second.index],
        )
    }
}

fn move_definition_rhs(
    graph: &mut Graph,
    definition: StatementLocation,
    usage: StatementLocation,
    variable: VariableId,
) -> bool {
    let (definition, usage) = statement_pair_mut(graph, definition, usage);
    let original = std::mem::replace(definition, Statement::Nop);
    let Statement::Assign { lhs, rhs, kind } = original else {
        *definition = original;
        return false;
    };
    if let Some(rhs) = usage.replace_once_owned(variable, rhs) {
        *definition = Statement::Assign { lhs, rhs, kind };
        false
    } else {
        true
    }
}

fn replace_with_definition_rhs(
    graph: &mut Graph,
    definition: StatementLocation,
    usage: StatementLocation,
    variable: VariableId,
) {
    let (definition, usage) = statement_pair_mut(graph, definition, usage);
    let rhs = definition
        .rhs()
        .expect("propagation origin has a right-hand side");
    usage.replace(variable, rhs);
}

pub fn place_declarations(graph: &mut Graph, chains: &DefUseChains) {
    let locations = LocationIndex::build(graph);
    place_declarations_at::<true, false>(graph, chains, &locations, None);
}

fn place_declarations_at<const PRESERVE_CHAINS: bool, const ENHANCED: bool>(
    graph: &mut Graph,
    chains: &DefUseChains,
    locations: &LocationIndex,
    live_locations: Option<&ActiveLocations>,
) {
    let dominators = graph.immediate_dominators();
    let mut variables = Vec::new();
    for rpo_index in (0..graph.rpo.len()).rev() {
        let node = graph.rpo[rpo_index];
        for &use_location in locations.node_locations(node) {
            if live_locations.is_some_and(|live| live.is_removed(use_location)) {
                continue;
            }
            let statement_location = locations.get(use_location);
            collect_unique_used_variables(
                &graph.nodes[statement_location.node].statements[statement_location.index],
                &mut variables,
            );
            for &variable in &variables {
                if graph.variables.get(variable).is_none() {
                    continue;
                }
                if graph.variables.is_declared(variable) {
                    continue;
                }
                let Some(definitions) = chains.use_def.get(&(variable, use_location)) else {
                    continue;
                };
                let mut definition_nodes = Vec::with_capacity(definitions.len());
                let mut catch_definition_nodes = Vec::new();
                for definition in definitions {
                    if let Definition::Instruction(location) = definition {
                        let definition_node = locations.node(*location);
                        if !graph.nodes[definition_node].in_catch {
                            definition_nodes.push(definition_node);
                        } else if ENHANCED {
                            catch_definition_nodes.push(definition_node);
                        }
                    }
                }
                definition_nodes.sort_unstable();
                definition_nodes.dedup();
                let Some(mut common) = definition_nodes.pop() else {
                    continue;
                };
                for definition_node in definition_nodes {
                    common = common_dominator(graph, &dominators, common, definition_node);
                }
                let defined_in_common = definitions.iter().any(|definition| match definition {
                    Definition::Instruction(location) => locations.node(*location) == common,
                    Definition::Parameter(_) => false,
                });
                let needs_hoisted_declaration = !defined_in_common
                    || (ENHANCED
                        && !catch_definition_nodes.is_empty()
                        && (graph.nodes[common].catch_source
                            || catch_definition_nodes
                                .iter()
                                .any(|&catch_node| catch_node != common)));
                if needs_hoisted_declaration && !graph.nodes[common].declarations.contains(&variable)
                {
                    graph.nodes[common].declarations.push(variable);
                }
            }
        }
    }
}

fn clear_path(
    graph: &Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
    variable: Option<VariableId>,
    from: Location,
    to: Location,
) -> bool {
    if from >= to {
        return false;
    }
    let from_location = locations.get(from);
    let to_location = locations.get(to);
    if from_location.node == to_location.node {
        return clear_statement_range(
            graph,
            locations,
            live_locations,
            from_location.node,
            from_location.index + 1,
            to_location.index,
            variable,
        );
    }
    if !clear_statement_range(
        graph,
        locations,
        live_locations,
        from_location.node,
        from_location.index + 1,
        graph.nodes[from_location.node].statements.len(),
        variable,
    ) {
        return false;
    }

    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([to_location.node]);
    while let Some(node) = queue.pop_front() {
        if node == from_location.node || !visited.insert(node) {
            continue;
        }
        let end = if node == to_location.node {
            to_location.index
        } else {
            graph.nodes[node].statements.len()
        };
        if !clear_statement_range(graph, locations, live_locations, node, 0, end, variable) {
            return false;
        }
        queue.extend(graph.all_predecessors(node));
    }
    true
}

fn clear_statement_range(
    graph: &Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
    node: NodeId,
    start: usize,
    end: usize,
    variable: Option<VariableId>,
) -> bool {
    graph.nodes[node].statements[start..end]
        .iter()
        .enumerate()
        .all(|(relative, statement)| {
            let index = start + relative;
            let location = locations.node_locations(node)[index];
            if live_locations.is_removed(location) {
                return true;
            }
            statement.defined_variable() != variable && !statement.has_side_effect()
        })
}

fn clear_variable_path(
    graph: &Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
    variable: VariableId,
    from: Location,
    to: Location,
) -> bool {
    if from >= to {
        return false;
    }
    let from_location = locations.get(from);
    let to_location = locations.get(to);
    if from_location.node == to_location.node {
        return clear_variable_statement_range(
            graph,
            locations,
            live_locations,
            from_location.node,
            from_location.index + 1,
            to_location.index,
            variable,
        );
    }
    if !clear_variable_statement_range(
        graph,
        locations,
        live_locations,
        from_location.node,
        from_location.index + 1,
        graph.nodes[from_location.node].statements.len(),
        variable,
    ) {
        return false;
    }

    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([to_location.node]);
    while let Some(node) = queue.pop_front() {
        if node == from_location.node || !visited.insert(node) {
            continue;
        }
        let end = if node == to_location.node {
            to_location.index
        } else {
            graph.nodes[node].statements.len()
        };
        if !clear_variable_statement_range(
            graph,
            locations,
            live_locations,
            node,
            0,
            end,
            variable,
        ) {
            return false;
        }
        queue.extend(graph.all_predecessors(node));
    }
    true
}

fn clear_variable_statement_range(
    graph: &Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
    node: NodeId,
    start: usize,
    end: usize,
    variable: VariableId,
) -> bool {
    graph.nodes[node].statements[start..end]
        .iter()
        .enumerate()
        .all(|(relative, statement)| {
            let index = start + relative;
            let location = locations.node_locations(node)[index];
            if live_locations.is_removed(location) {
                return true;
            }
            statement.defined_variable() != Some(variable)
        })
}

fn common_dominator(
    graph: &Graph,
    dominators: &[Option<NodeId>],
    mut left: NodeId,
    mut right: NodeId,
) -> NodeId {
    while left != right {
        while graph.nodes[left].rpo > graph.nodes[right].rpo {
            left = dominators[left].expect("reachable node has a dominator");
        }
        while graph.nodes[right].rpo > graph.nodes[left].rpo {
            right = dominators[right].expect("reachable node has a dominator");
        }
    }
    left
}

fn compact_removed_without_index(
    graph: &mut Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
) {
    for &node in &graph.rpo {
        let mut retained = Vec::with_capacity(graph.nodes[node].statements.len());
        for (index, statement) in graph.nodes[node].statements.drain(..).enumerate() {
            let location = locations.node_locations(node)[index];
            if !live_locations.is_removed(location) {
                retained.push(statement);
            }
        }
        graph.nodes[node].statements = retained;
    }
}

fn compact_removed(
    graph: &mut Graph,
    locations: &LocationIndex,
    live_locations: &ActiveLocations,
) -> LocationIndex {
    let mut compacted = LocationIndex {
        statements: Vec::with_capacity(live_locations.total_count()),
        by_node: vec![Vec::new(); graph.nodes.len()],
        has_instruction_definitions: locations.has_instruction_definitions,
    };
    for &node in &graph.rpo {
        let mut retained = Vec::with_capacity(graph.nodes[node].statements.len());
        for (index, statement) in graph.nodes[node].statements.drain(..).enumerate() {
            let location = locations.node_locations(node)[index];
            if !live_locations.is_removed(location) {
                compacted.by_node[node].push(compacted.statements.len());
                compacted.statements.push(StatementLocation {
                    node,
                    index: retained.len(),
                });
                retained.push(statement);
            }
        }
        graph.nodes[node].statements = retained;
    }
    compacted
}

fn definition_sort_key(definition: &Definition) -> (u8, usize) {
    match definition {
        Definition::Parameter(_) => (0, 0),
        Definition::Instruction(location) => (1, *location),
    }
}

fn fold_new_instance_constructors(graph: &mut Graph) {
    let mut used = Vec::new();
    for rpo_index in 0..graph.rpo.len() {
        let node = graph.rpo[rpo_index];
        let mut index = 0;
        while index < graph.nodes[node].statements.len() {
            let new_var_and_ty = match &graph.nodes[node].statements[index] {
                Statement::Assign {
                    lhs: Some(lhs),
                    rhs: Value::NewInstance { ty },
                    ..
                } => Some((*lhs, ty.clone())),
                _ => None,
            };
            let Some((new_var, new_ty)) = new_var_and_ty else {
                index += 1;
                continue;
            };
            let mut cursor = index + 1;
            let mut folded = false;
            while cursor < graph.nodes[node].statements.len() {
                let statement = &graph.nodes[node].statements[cursor];
                if let Statement::Assign {
                    lhs,
                    rhs: Value::Invoke(invoke),
                    ..
                } = statement
                    && (lhs.is_none() || *lhs == Some(new_var))
                    && invoke.method.name == "<init>"
                    && invoke.method.class == new_ty
                    && matches!(&invoke.base, Value::Variable(base) if *base == new_var)
                {
                    let arg_mentions_new_var = invoke.arguments.iter().any(|argument| {
                        let mut arg_vars = HashSet::new();
                        argument.used_variables(&mut arg_vars);
                        arg_vars.contains(&new_var)
                    });
                    if !arg_mentions_new_var {
                        if let Statement::Assign {
                            lhs,
                            rhs: Value::Invoke(invoke),
                            ..
                        } = &mut graph.nodes[node].statements[cursor]
                        {
                            *lhs = Some(new_var);
                            invoke.base = Value::NewInstance { ty: new_ty };
                        }
                        graph.nodes[node].statements.remove(index);
                        folded = true;
                    }
                    break;
                }
                used.clear();
                statement.collect_used_variables_vec(&mut used);
                if used.contains(&new_var) || statement.defined_variable() == Some(new_var) {
                    break;
                }
                cursor += 1;
            }
            if !folded {
                index += 1;
            }
        }
    }
}

fn fold_string_builders(graph: &mut Graph) {
    let mut used = Vec::new();
    for rpo_index in 0..graph.rpo.len() {
        let node = graph.rpo[rpo_index];
        let mut index = 0;
        while index < graph.nodes[node].statements.len() {
            let init_var = match &graph.nodes[node].statements[index] {
                Statement::Assign {
                    lhs: Some(lhs),
                    rhs: Value::Invoke(invoke),
                    ..
                } if invoke.method.class == "Ljava/lang/StringBuilder;"
                    && invoke.method.name == "<init>"
                    && matches!(&invoke.base, Value::NewInstance { ty } if ty == "Ljava/lang/StringBuilder;")
                    && (invoke.arguments.is_empty()
                        || (invoke.arguments.len() == 1
                            && matches!(
                                invoke.method.parameters.first().map(String::as_str),
                                Some("I" | "Ljava/lang/String;" | "Ljava/lang/CharSequence;")
                            ))) =>
                {
                    Some(*lhs)
                }
                _ => None,
            };
            let Some(sb_var) = init_var else {
                index += 1;
                continue;
            };
            let mut append_count = 0;
            while index + 1 + append_count < graph.nodes[node].statements.len() {
                let is_append = match &graph.nodes[node].statements[index + 1 + append_count] {
                    Statement::Assign {
                        lhs,
                        rhs: Value::Invoke(invoke),
                        ..
                    } if (lhs.is_none() || *lhs == Some(sb_var))
                        && invoke.method.class == "Ljava/lang/StringBuilder;"
                        && invoke.method.name == "append"
                        && invoke.arguments.len() == 1
                        && matches!(&invoke.base, Value::Variable(base) if *base == sb_var) =>
                    {
                        let mut arg_vars = HashSet::new();
                        invoke.arguments[0].used_variables(&mut arg_vars);
                        !arg_vars.contains(&sb_var)
                    }
                    _ => false,
                };
                if is_append {
                    append_count += 1;
                } else {
                    break;
                }
            }
            if append_count == 0 || index + 1 + append_count >= graph.nodes[node].statements.len() {
                index += 1;
                continue;
            }
            let target_idx = index + 1 + append_count;
            used.clear();
            graph.nodes[node].statements[target_idx].collect_used_variables_vec(&mut used);
            if !used.contains(&sb_var) {
                index += 1;
                continue;
            }
            let used_later_in_block = graph.nodes[node].statements[target_idx + 1..]
                .iter()
                .any(|stmt| {
                    let mut v = Vec::new();
                    stmt.collect_used_variables_vec(&mut v);
                    v.contains(&sb_var) || stmt.defined_variable() == Some(sb_var)
                });
            let used_in_other_blocks = graph.rpo.iter().any(|&other_node| {
                other_node != node
                    && graph.nodes[other_node].statements.iter().any(|stmt| {
                        let mut v = Vec::new();
                        stmt.collect_used_variables_vec(&mut v);
                        v.contains(&sb_var)
                    })
            });
            if used_later_in_block || used_in_other_blocks {
                index += 1;
                continue;
            }
            let Statement::Assign {
                rhs: init_value, ..
            } = graph.nodes[node].statements.remove(index)
            else {
                unreachable!()
            };
            let mut chain = init_value;
            for _ in 0..append_count {
                let Statement::Assign {
                    rhs: Value::Invoke(mut append_invoke),
                    ..
                } = graph.nodes[node].statements.remove(index)
                else {
                    unreachable!()
                };
                append_invoke.base = chain;
                chain = Value::Invoke(append_invoke);
            }
            graph.nodes[node].statements[index].replace(sb_var, &chain);
            for n in &mut graph.nodes {
                n.declarations.retain(|decl| *decl != sb_var);
            }
        }
    }
}

fn eliminate_identity_copies(graph: &mut Graph) {
    for rpo_index in 0..graph.rpo.len() {
        let node = graph.rpo[rpo_index];
        let mut index = 0;
        while index < graph.nodes[node].statements.len() {
            let copy_pair = match &graph.nodes[node].statements[index] {
                Statement::Assign {
                    lhs: Some(lhs),
                    rhs: Value::Variable(src),
                    kind: AssignKind::Move | AssignKind::Normal,
                } if lhs != src
                    && !graph.variables.is_declared(*lhs)
                    && !graph.variables.is_this(*lhs)
                    && !graph.variables.is_super(*lhs) =>
                {
                    let lhs_ty = graph.variables.get(*lhs).and_then(|v| v.ty.as_deref());
                    let src_ty = graph.variables.get(*src).and_then(|v| v.ty.as_deref());
                    if lhs_ty == src_ty || lhs_ty.is_none() || lhs_ty == Some("Ljava/lang/Object;")
                    {
                        Some((*lhs, *src, false))
                    } else {
                        None
                    }
                }
                Statement::Assign {
                    lhs: Some(lhs),
                    rhs: Value::CheckCast(cast),
                    kind: AssignKind::Move | AssignKind::Normal,
                } if cast.descriptor == "Ljava/lang/Iterable;"
                    && matches!(&cast.value, Value::Variable(src) if src != lhs)
                    && !graph.variables.is_declared(*lhs)
                    && !graph.variables.is_this(*lhs)
                    && !graph.variables.is_super(*lhs) =>
                {
                    let Value::Variable(src) = &cast.value else {
                        unreachable!()
                    };
                    Some((*lhs, *src, true))
                }
                _ => None,
            };
            let Some((dst, src, is_iterable_cast)) = copy_pair else {
                index += 1;
                continue;
            };
            let other_defs_or_src_redef = graph.rpo.iter().any(|&nid| {
                graph.nodes[nid]
                    .statements
                    .iter()
                    .enumerate()
                    .any(|(sidx, stmt)| {
                        if nid == node && sidx == index {
                            return false;
                        }
                        let def = stmt.defined_variable();
                        def == Some(dst) || def == Some(src)
                    })
            });
            if other_defs_or_src_redef {
                index += 1;
                continue;
            }
            if is_iterable_cast {
                let only_used_in_iterator = graph.rpo.iter().all(|&nid| {
                    graph.nodes[nid]
                        .statements
                        .iter()
                        .enumerate()
                        .all(|(sidx, stmt)| {
                            if nid == node && sidx == index {
                                return true;
                            }
                            let mut used = Vec::new();
                            stmt.collect_used_variables_vec(&mut used);
                            if !used.contains(&dst) {
                                return true;
                            }
                            matches!(
                                stmt,
                                Statement::Assign {
                                    rhs: Value::Invoke(inv),
                                    ..
                                } if inv.method.name == "iterator"
                                    && inv.arguments.is_empty()
                                    && matches!(&inv.base, Value::Variable(v) if *v == dst)
                            )
                        })
                });
                if !only_used_in_iterator {
                    index += 1;
                    continue;
                }
            }
            graph.nodes[node].statements.remove(index);
            let replacement = Value::Variable(src);
            for &nid in &graph.rpo {
                for stmt in &mut graph.nodes[nid].statements {
                    stmt.replace(dst, &replacement);
                }
                graph.nodes[nid].declarations.retain(|d| *d != dst);
            }
        }
    }
}

fn recover_types_and_constants(graph: &mut Graph, return_type: &str) {
    fold_new_instance_constructors(graph);
    fold_string_builders(graph);
    for _ in 0..2 {
        let mut changed = false;
        for &node in &graph.rpo {
            for index in 0..graph.nodes[node].statements.len() {
                match &graph.nodes[node].statements[index] {
                    Statement::Assign {
                        lhs: Some(lhs),
                        rhs,
                        ..
                    } => {
                        let lhs = *lhs;
                        let current_ty = graph
                            .variables
                            .get(lhs)
                            .and_then(|info| info.ty.as_deref());
                        let is_compare = matches!(
                            rhs,
                            Value::Binary(binary) if binary.op == BinaryOp::Compare
                        );
                        if is_compare && !graph.variables.is_declared(lhs) {
                            if current_ty != Some("I")
                                && let Some(var) = graph.variables.get_mut(lhs)
                            {
                                var.ty = Some(Cow::Borrowed("I"));
                                changed = true;
                            }
                        } else if !graph.variables.is_declared(lhs)
                            && let Some(inferred) = (if let Value::Invoke(invoke) = rhs
                                && invoke.method.name == "<init>"
                                && invoke.method.return_type == "V"
                            {
                                value_type(&invoke.base, graph)
                            } else {
                                value_type(rhs, graph)
                            })
                            && !matches!(inferred.as_ref(), "V" | "U" | "W" | "O" | "unknownType")
                            && (current_ty.is_none_or(|ty| {
                                matches!(ty, "V" | "U" | "W" | "O" | "unknownType")
                            }) || (current_ty == Some("I")
                                && (inferred.starts_with('L')
                                    || inferred.starts_with('[')
                                    || inferred.as_ref() == "Z"))
                                || (current_ty == Some("Ljava/lang/Object;")
                                    && (inferred.starts_with('L') || inferred.starts_with('['))
                                    && inferred.as_ref() != "Ljava/lang/Object;"))
                        {
                            let has_conflicting_non_zero_int = (inferred.starts_with('L')
                                || inferred.starts_with('[')
                                || inferred.as_ref() == "Z")
                                && graph.rpo.iter().any(|&nid| {
                                    graph.nodes[nid].statements.iter().any(|stmt| {
                                        if let Statement::Assign {
                                            lhs: Some(other_lhs),
                                            rhs: other_rhs,
                                            ..
                                        } = stmt
                                            && *other_lhs == lhs
                                        {
                                            if matches!(
                                                other_rhs,
                                                Value::Constant(Constant {
                                                    value: ConstantValue::Integer { value: 0, ty: "I" },
                                                })
                                            ) {
                                                return false;
                                            }
                                            if inferred.as_ref() == "Z"
                                                && matches!(
                                                    other_rhs,
                                                    Value::Constant(Constant {
                                                        value: ConstantValue::Integer { value: 1, ty: "I" },
                                                    })
                                                )
                                            {
                                                return false;
                                            }
                                            value_type(other_rhs, graph).as_deref() == Some("I")
                                        } else {
                                            false
                                        }
                                    })
                                });
                            if !has_conflicting_non_zero_int
                                && let Some(var) = graph.variables.get_mut(lhs)
                            {
                                var.ty = Some(inferred);
                                changed = true;
                            }
                        }
                    }
                    Statement::FillArray {
                        array: Value::Variable(var),
                        element_width,
                        ..
                    } => {
                        let var = *var;
                        let current_ty = graph
                            .variables
                            .get(var)
                            .and_then(|info| info.ty.as_deref());
                        if current_ty.is_none_or(|ty| matches!(ty, "U" | "W" | "O" | "unknownType")) {
                            let inferred = match *element_width {
                                1 => Some(Cow::Borrowed("[B")),
                                2 => Some(Cow::Borrowed("[S")),
                                4 => Some(Cow::Borrowed("[I")),
                                8 => Some(Cow::Borrowed("[J")),
                                _ => None,
                            };
                            if let Some(inferred) = inferred
                                && let Some(info) = graph.variables.get_mut(var)
                            {
                                info.ty = Some(inferred);
                                changed = true;
                            }
                        }
                    }
                    Statement::Return(Some(Value::Variable(var))) => {
                        let var = *var;
                        let current_ty = graph
                            .variables
                            .get(var)
                            .and_then(|info| info.ty.as_deref());
                        if current_ty.is_none_or(|ty| {
                            matches!(ty, "U" | "W" | "O" | "unknownType")
                                || (ty == "I" && matches!(return_type, "Z" | "F" | "B" | "S" | "C"))
                                || (ty == "J" && return_type == "D")
                        }) && !graph.variables.is_declared(var)
                            && return_type != "V"
                            && let Some(info) = graph.variables.get_mut(var)
                        {
                            info.ty = Some(Cow::Owned(return_type.to_owned()));
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            break;
        }
    }
    eliminate_identity_copies(graph);

    for _ in 0..2 {
        for rpo_index in 0..graph.rpo.len() {
            let node = graph.rpo[rpo_index];
            for index in 0..graph.nodes[node].statements.len() {
                let mut statement = std::mem::replace(
                    &mut graph.nodes[node].statements[index],
                    Statement::Nop,
                );
                propagate_statement_expected_types(&mut statement, graph, return_type);
                graph.nodes[node].statements[index] = statement;
            }
        }
        for &node in &graph.rpo {
            for index in 0..graph.nodes[node].statements.len() {
                if let Statement::Assign {
                    lhs: Some(lhs),
                    rhs,
                    ..
                } = &graph.nodes[node].statements[index]
                {
                    let lhs = *lhs;
                    if !graph.variables.is_declared(lhs)
                        && graph.variables.get(lhs).and_then(|v| v.ty.as_deref()).is_none()
                        && let Some(inferred) = value_type(rhs, graph)
                        && !matches!(inferred.as_ref(), "V" | "U" | "W" | "O" | "unknownType")
                        && let Some(var) = graph.variables.get_mut(lhs)
                    {
                        var.ty = Some(inferred);
                    }
                }
            }
        }
    }
}

fn propagate_statement_expected_types(
    statement: &mut Statement,
    graph: &mut Graph,
    return_type: &str,
) {
    match statement {
        Statement::Assign { lhs, rhs, .. } => {
            let expected = lhs
                .and_then(|variable| graph.variables.get(variable))
                .and_then(|info| info.ty.as_deref())
                .map(str::to_owned);
            propagate_value_expected_type(rhs, expected.as_deref(), graph);
        }
        Statement::ArrayStore(store) => {
            propagate_value_expected_type(&mut store.array, None, graph);
            propagate_value_expected_type(&mut store.index, Some("I"), graph);
            let element_ty = value_type(&store.array, graph)
                .and_then(|ty| ty.strip_prefix('[').map(|s| s.to_owned()))
                .or_else(|| store.ty.map(str::to_owned));
            propagate_value_expected_type(&mut store.value, element_ty.as_deref(), graph);
        }
        Statement::StaticPut { value, field } => {
            let field_ty = field.ty.to_string();
            propagate_value_expected_type(value, Some(&field_ty), graph);
        }
        Statement::InstancePut(store) => {
            let class_ty = store.field.class.to_string();
            let field_ty = store.field.ty.to_string();
            propagate_value_expected_type(&mut store.object, Some(&class_ty), graph);
            propagate_value_expected_type(&mut store.value, Some(&field_ty), graph);
        }
        Statement::Return(Some(value)) => {
            propagate_value_expected_type(value, Some(return_type), graph);
        }
        Statement::MonitorEnter(value) | Statement::MonitorExit(value) => {
            propagate_value_expected_type(value, Some("Ljava/lang/Object;"), graph);
        }
        Statement::Throw(value) => {
            propagate_value_expected_type(value, Some("Ljava/lang/Throwable;"), graph);
        }
        Statement::Switch { value, .. } => {
            propagate_value_expected_type(value, Some("I"), graph);
        }
        Statement::FillArray { array, .. } => {
            propagate_value_expected_type(array, None, graph);
        }
        Statement::Condition { left, right, .. } => {
            if let Some(right) = right {
                let left_ty = value_type(left, graph).map(|s| s.into_owned());
                let right_ty = value_type(right, graph).map(|s| s.into_owned());
                propagate_value_expected_type(left, right_ty.as_deref(), graph);
                propagate_value_expected_type(right, left_ty.as_deref(), graph);
            } else {
                propagate_value_expected_type(left, None, graph);
            }
        }
        Statement::Return(None) | Statement::Nop | Statement::MoveException { .. } => {}
    }
}

fn propagate_value_expected_type(
    value: &mut Value,
    expected: Option<&str>,
    graph: &mut Graph,
) {
    match value {
        Value::Variable(variable) => {
            if let Some(expected) = expected
                && !graph.variables.is_declared(*variable)
            {
                let current_ty = graph
                    .variables
                    .get(*variable)
                    .and_then(|info| info.ty.as_deref());
                let new_ty = match (current_ty, expected) {
                    (Some("I"), "F") => Some(Cow::Borrowed("F")),
                    (Some("J"), "D") => Some(Cow::Borrowed("D")),
                    (Some("I"), "Z") => Some(Cow::Borrowed("Z")),
                    (Some("I"), "C") => Some(Cow::Borrowed("C")),
                    (None | Some("U" | "W" | "O" | "unknownType"), other)
                        if !matches!(other, "V" | "U" | "W" | "O" | "unknownType") =>
                    {
                        Some(Cow::Owned(other.to_owned()))
                    }
                    _ => None,
                };
                if let Some(new_ty) = new_ty
                    && let Some(info) = graph.variables.get_mut(*variable)
                {
                    info.ty = Some(new_ty);
                }
            }
        }
        Value::Constant(Constant {
            value: ConstantValue::Integer { value, ty },
        }) => {
            if let Some(expected) = expected {
                match (*ty, expected) {
                    ("I", "F") => *ty = "F",
                    ("J", "D") => *ty = "D",
                    ("I", "Z") => *ty = "Z",
                    ("I", "C") => *ty = "C",
                    ("I", other)
                        if *value == 0
                            && (other.starts_with('L')
                                || other.starts_with('[')
                                || other == "O") =>
                    {
                        *ty = "L";
                    }
                    _ => {}
                }
            }
        }
        Value::Invoke(invoke) => {
            let class_ty = invoke.method.class.clone();
            propagate_value_expected_type(&mut invoke.base, Some(&class_ty), graph);
            for (argument, param_ty) in invoke
                .arguments
                .iter_mut()
                .zip(invoke.method.parameters.iter())
            {
                propagate_value_expected_type(argument, Some(param_ty.as_str()), graph);
            }
        }
        Value::CheckCast(cast) => {
            let descriptor = cast.descriptor.clone();
            propagate_value_expected_type(&mut cast.value, Some(&descriptor), graph);
        }
        Value::Unary(unary) => {
            let operand_expected = if unary.op == crate::ir::UnaryOp::Cast {
                None
            } else {
                Some(unary.ty)
            };
            propagate_value_expected_type(&mut unary.value, operand_expected, graph);
        }
        Value::ArrayLoad(load) => {
            propagate_value_expected_type(&mut load.array, None, graph);
            propagate_value_expected_type(&mut load.index, Some("I"), graph);
        }
        Value::ArrayLength(array) => {
            propagate_value_expected_type(array, None, graph);
        }
        Value::NewArray(array) => {
            propagate_value_expected_type(&mut array.size, Some("I"), graph);
        }
        Value::FilledArray(array) => {
            let element_ty = array.ty.strip_prefix('[').map(str::to_owned);
            for argument in &mut array.arguments {
                propagate_value_expected_type(argument, element_ty.as_deref(), graph);
            }
        }
        Value::Binary(binary) => {
            let (left_expected, right_expected) = match binary.op {
                BinaryOp::ShiftLeft | BinaryOp::ShiftRight | BinaryOp::UnsignedShiftRight => {
                    (Some(binary.ty), Some("I"))
                }
                BinaryOp::InstanceOf => (None, None),
                _ => (Some(binary.ty), Some(binary.ty)),
            };
            propagate_value_expected_type(&mut binary.left, left_expected, graph);
            propagate_value_expected_type(&mut binary.right, right_expected, graph);
        }
        Value::InstanceField(field) => {
            let class_ty = field.field.class.to_string();
            propagate_value_expected_type(&mut field.object, Some(&class_ty), graph);
        }
        Value::Constant(_)
        | Value::BaseClass { .. }
        | Value::NewInstance { .. }
        | Value::StaticField(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_word_range_mask_preserves_exact_bits_ut() {
        let mut words = Vec::new();
        append_word_range_mask(&mut words, 3..5);
        assert_eq!(words, vec![(0, 0b11 << 3)]);

        words.clear();
        append_word_range_mask(&mut words, 60..132);
        assert_eq!(words, vec![(0, !((1_u64 << 60) - 1)), (1, u64::MAX), (2, 0b1111)]);

        append_word_range_mask(&mut words, 12..12);
        assert_eq!(words, vec![(0, !((1_u64 << 60) - 1)), (1, u64::MAX), (2, 0b1111)]);
    }

    #[test]
    fn clear_bit_range_preserves_boundary_bits_ut() {
        let mut bits = [u64::MAX; 3];
        clear_bit_range(&mut bits, 3..5);
        assert_eq!(bits, [u64::MAX ^ (0b11 << 3), u64::MAX, u64::MAX]);

        let mut spanning_bits = [u64::MAX; 3];
        clear_bit_range(&mut spanning_bits, 60..132);
        assert_eq!(spanning_bits[0], (1_u64 << 60) - 1);
        assert_eq!(spanning_bits[1], 0);
        assert_eq!(spanning_bits[2], !0b1111);

        let before = bits;
        clear_bit_range(&mut bits, 12..12);
        assert_eq!(bits, before);
    }

    #[test]
    fn parameter_only_chains_have_no_instruction_definitions() {
        let variable = VariableId::Register(0);
        let parameter = Definition::Parameter(variable);
        let instruction = Definition::Instruction(1);
        let mut chains = DefUseChains::default();

        chains.def_use.insert((variable, parameter), vec![0]);
        chains.use_def.insert((variable, 0), vec![parameter]);
        assert!(!chains_have_instruction_definitions(&chains));

        chains.def_use.insert((variable, instruction), vec![2]);
        chains.use_def.insert((variable, 2), vec![instruction]);
        assert!(chains_have_instruction_definitions(&chains));
    }
}

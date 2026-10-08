//! The control state machine: what decides when he attacks, dodges, climbs, jumps or changes
//! form. In the original this is data (`bnxglobal.str`, the `state*`, `connection*` and
//! `stateMachine*` objects), read here as it stands; the code below is the original's rules
//! for evaluating it (`notes\state-machine.md`).
//!
//! Tags as in `sim.rs`: `[game]` read from the original's code, `[data]` straight from its
//! data, `[assumed]` a reading that has not been checked, `[stand-in]` invented.

use std::collections::HashMap;

use glam::Vec2;

use crate::formats::hash::crc32;
use crate::formats::lxb::{DataFile, Node};

/// The game's button numbers, as its detectors store them. [game]
pub mod button {
    pub const ACTION: u32 = 0x00;
    pub const ATTACK_FAST: u32 = 0x01;
    pub const ATTACK_SPECIAL: u32 = 0x02;
    pub const ATTACK_VEHICLE: u32 = 0x03;
    pub const CLIMB: u32 = 0x0b;
    pub const DODGE: u32 = 0x0c;
    pub const JUMP: u32 = 0x0e;
    pub const VEHICLE_MODE: u32 = 0x15;
    pub const WEAPON_FIRE: u32 = 0x17;
    pub const WEAPON_MODE: u32 = 0x18;
    /// `AttackTransformJump`, which the data binds to the jump button: the leap out of the car.
    pub const TRANSFORM_JUMP: u32 = 0x22;
    /// `AttackTransformPunch` (the data binds it to the action and climb button) and
    /// `AttackTransformSlam` (to the melee button): the two attacks out of the car.
    pub const TRANSFORM_PUNCH: u32 = 0x23;
    pub const TRANSFORM_SLAM: u32 = 0x24;
    /// One past the highest button a detector may name.
    pub const COUNT: u32 = 0x29;
}

/// Environment tests a connection may ask for, by the hash the data stores. What each one
/// looks at in the original is not read; the names are the connections'. [data]
pub mod env {
    pub const CAN_SWITCH_AVATAR: u32 = 0x52be_b236;
    pub const CLIMBABLE: u32 = 0x96c5_dbae;
    pub const NOT_HOLD_VIP: u32 = 0x14fd_46e2;
    pub const INSIDE_FLIGHT_BOUNDARY: u32 = 0xf65c_039d;
}

/// The pad as the detectors see it for one update.
#[derive(Clone, Copy, Default, Debug)]
pub struct Pad {
    /// One bit per button number: down now, and went down this update.
    pub down: u64,
    pub hit: u64,
    /// Left stick against the camera, and against the character's facing: x right, y forward.
    pub left: Vec2,
    pub left_character: Vec2,
}

impl Pad {
    pub fn set(&mut self, button: u32, down: bool, hit: bool) {
        self.down |= (down as u64) << button;
        self.hit |= (hit as u64) << button;
    }

    pub fn is_down(&self, button: u32) -> bool {
        self.down >> button & 1 == 1
    }

    pub fn was_hit(&self, button: u32) -> bool {
        self.hit >> button & 1 == 1
    }
}

#[derive(Clone, Debug)]
pub struct StickRange {
    pub amp_min: f32,
    pub amp_max: f32,
    /// Radians, 0 forward and positive to the left.
    pub angle_min: f32,
    pub angle_max: f32,
    pub camera_relative: bool,
}

#[derive(Clone, Debug)]
pub struct Stage {
    /// Seconds that must have passed since the stage before, and seconds the buttons in
    /// `include` must have been held.
    pub time: f32,
    pub charge: f32,
    pub include: Vec<u32>,
    pub exclude: Vec<u32>,
    pub hit: Option<u32>,
    pub left: StickRange,
}

impl Stage {
    /// The stage's own test for this update. [game]
    fn passes(&self, pad: &Pad) -> bool {
        if self.hit.is_some_and(|b| !pad.was_hit(b))
            || self.include.iter().any(|&b| !pad.is_down(b))
            || self.exclude.iter().any(|&b| pad.is_down(b))
        {
            return false;
        }
        let amp = pad.left.length();
        if amp < self.left.amp_min || amp > self.left.amp_max {
            return false;
        }
        let stick = if self.left.camera_relative { pad.left } else { pad.left_character };
        let mut angle = (-stick.x).atan2(stick.y);
        if self.left.angle_max >= 0.0 {
            if self.left.angle_min > 0.0 && angle < 0.0 {
                angle += std::f32::consts::TAU;
            }
        } else if angle > 0.0 {
            angle -= std::f32::consts::TAU;
        }
        self.left.angle_min <= angle && angle <= self.left.angle_max
    }
}

#[derive(Clone, Debug, Default)]
pub struct Detector {
    pub stages: Vec<Stage>,
    pub stays_on: f32,
    pub hold_with_last_stage: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DetectorState {
    stage_time: f32,
    held_time: f32,
    stage: usize,
    pub active: bool,
    active_left: f32,
}

impl Detector {
    /// One update of the detector. [game]
    pub fn update(&self, state: &mut DetectorState, pad: &Pad, dt: f32) {
        state.stage_time += dt;
        let current = self.stages.get(state.stage);
        let held = current.is_none_or(|stage| stage.include.iter().all(|&b| pad.is_down(b)));
        state.held_time = if held { state.held_time + dt } else { 0.0 };
        if let Some(stage) = current.filter(|stage| stage.passes(pad)) {
            if stage.charge <= state.held_time && stage.time <= state.stage_time {
                state.stage_time -= stage.time;
                state.held_time -= stage.charge;
                state.stage += 1;
            }
        }
        if state.stage >= self.stages.len() {
            *state = DetectorState { active: true, active_left: self.stays_on, ..Default::default() };
        }
        if state.active {
            if self.hold_with_last_stage {
                // [assumed] which stage is held to: the name says the last.
                if self.stages.last().is_some_and(|stage| !stage.passes(pad)) {
                    state.active = false;
                    state.active_left = 0.0;
                }
            } else {
                state.active_left -= dt;
                if state.active_left <= 0.0 {
                    state.active = false;
                    state.active_left = 0.0;
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Test {
    /// Only the checks every connection has.
    None,
    Detect(Detector),
    /// [assumed] from the names: height over the ground against a limit.
    DistToGround { limit: f32, greater: bool },
    /// [assumed] from the name: seconds in the state.
    TimeInState(f32),
    Environment(u32),
    Message(u32),
    /// A connection list: all of them.
    All(Vec<usize>),
}

#[derive(Clone, Debug)]
pub struct Connection {
    pub name: String,
    pub in_air: bool,
    pub on_ground: bool,
    pub holding: bool,
    pub not_holding: bool,
    pub min_z_velocity: f32,
    pub max_z_velocity: f32,
    pub branch_begin: i32,
    pub branch_end: i32,
    pub attack_can_branch: bool,
    pub can_adv_transform: bool,
    pub test: Test,
}

impl Connection {
    fn open(name: String, test: Test) -> Self {
        Self {
            name,
            in_air: false,
            on_ground: false,
            holding: false,
            not_holding: false,
            min_z_velocity: f32::MIN,
            max_z_velocity: f32::MAX,
            branch_begin: 0,
            branch_end: 0,
            attack_can_branch: false,
            can_adv_transform: false,
            test,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub connection: usize,
    /// `None` in the data means "back to where the state was entered from".
    pub output: Option<usize>,
    pub priority: i32,
}

#[derive(Clone, Debug, Default)]
pub struct StateDef {
    pub name: String,
    /// The object's class: `StateAttack`, `StateDash`, `StateBasic`...
    pub class: String,
    pub overlay: bool,
    /// Forced-message state permissions (death, full hit, partial hit, stun, grab).
    /// [game: FUN_00850c80 reads State +0x40; data field canGoToStates]
    pub can_go_to: u32,
    /// CRC-32 of the clip the state plays, 0 for none, and of its set when it names one.
    pub anim: u32,
    pub anim_set: Option<String>,
    /// The motion mode the state sets, by class id.
    pub motion_mode: u32,
    pub finish_on_set_exit: bool,
    /// `attackRanged` and `fireButton`: an attack state that works the weapon instead of
    /// playing a clip, and the button it watches.
    pub ranged: bool,
    pub fire_button: u32,
    /// `robotForm`: the control mode the state puts the character in as it is entered
    /// (`FUN_0071ab70`: 1 on foot, 2 weapon, 3 the car), or 5 to leave it as it is; the
    /// drive states set 3 and the weapon states 2 in their own code. [game] The ways out
    /// of the car (the unfold, the slam, the punch) all have 1.
    pub robot_form: u32,
    /// Every machine's transitions out of this state, in the order the machines list them.
    pub transitions: Vec<Transition>,
}

/// What a connection's checks look at, for one update.
pub struct Facts<'a> {
    pub on_ground: bool,
    pub z_velocity: f32,
    pub holding: bool,
    /// Branch points passed in the playing clip, counting from 1; 100 once it has finished.
    pub branch: i32,
    pub attack_can_branch: bool,
    pub can_adv_transform: bool,
    pub ground_distance: f32,
    pub env: &'a dyn Fn(u32) -> bool,
    pub messages: &'a [u32],
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub states: Vec<StateDef>,
    pub connections: Vec<Connection>,
    by_name: HashMap<String, usize>,
}

fn flag(node: Node, name: &str) -> bool {
    node.get(name).is_some_and(|n| n.int() == Some(1) || n.is_true())
}

fn num(node: Node, name: &str, default: f32) -> f32 {
    node.get(name).and_then(Node::float).unwrap_or(default)
}

fn name_of(file: &DataFile, node: Node) -> String {
    let label = node.label().unwrap_or(0);
    file.name(label).map_or_else(|| format!("#{label:08x}"), str::to_owned)
}

fn read_stick(node: Option<Node>) -> StickRange {
    let get = |name: &str, default: f32| node.map_or(default, |n| num(n, name, default));
    StickRange {
        amp_min: get("AmpMin", -1.0),
        amp_max: get("AmpMax", 100.0),
        angle_min: get("AngleMin", -360.0).to_radians(),
        angle_max: get("AngleMax", 360.0).to_radians(),
        camera_relative: node.is_none_or(|n| flag(n, "cameraRelative")),
    }
}

fn read_detector(node: Node) -> Detector {
    let buttons = |stage: Node, name: &str| -> Vec<u32> {
        stage.get(name).map(Node::ints).unwrap_or_default().into_iter().map(|b| b as u32).collect()
    };
    Detector {
        stages: node
            .get("stages")
            .into_iter()
            .flat_map(Node::items)
            .map(|stage| Stage {
                time: num(stage, "time", 0.0),
                charge: num(stage, "timeIncludeButtonCharge", 0.0),
                include: buttons(stage, "includeButtons"),
                exclude: buttons(stage, "excludeButtons"),
                hit: stage.get("includeHitButton").and_then(Node::int).map(|b| b as u32).filter(|&b| b < button::COUNT),
                left: read_stick(stage.get("LStick")),
            })
            .collect(),
        stays_on: num(node, "timeActivationStaysOn", 0.0),
        hold_with_last_stage: flag(node, "holdWithLastStage"),
    }
}

impl Graph {
    /// Reads a character's state machines: `characterAttributes<Name>.stateMachineList`.
    pub fn read(file: &DataFile, list: Node) -> Self {
        let mut graph = Graph::default();
        let mut state_of: HashMap<u32, usize> = HashMap::new();
        let mut connection_of: HashMap<u32, usize> = HashMap::new();
        for machine in list.get("stateMachines").into_iter().flat_map(Node::items) {
            for entry in machine.get("states").into_iter().flat_map(Node::items) {
                let Some(state) = entry.get("state").filter(|s| s.label().is_some()) else { continue };
                let from = graph.state(file, state, &mut state_of);
                for link in entry.get("connections").into_iter().flat_map(Node::items) {
                    let via = link.get("connection").filter(|c| c.label().is_some()).or_else(|| link.get("connectionList"));
                    let Some(via) = via.filter(|c| c.label().is_some()) else { continue };
                    let connection = graph.connection(file, via, &mut connection_of);
                    let output = link
                        .get("outputState")
                        .filter(|s| s.label().is_some())
                        .map(|s| graph.state(file, s, &mut state_of));
                    let priority = link.get("priority").and_then(Node::int).unwrap_or(0) as i32;
                    graph.states[from].transitions.push(Transition { connection, output, priority });
                }
            }
        }
        graph
    }

    fn state(&mut self, file: &DataFile, node: Node, seen: &mut HashMap<u32, usize>) -> usize {
        let label = node.label().unwrap_or(0);
        if let Some(&index) = seen.get(&label) {
            return index;
        }
        let anim = node.get("animName").and_then(|n| {
            n.int().map(|h| h as u32).or_else(|| n.text().filter(|t| !t.is_empty()).map(|t| crc32(t.as_bytes())))
        });
        let name = name_of(file, node);
        self.states.push(StateDef {
            name: name.clone(),
            class: node.type_name().unwrap_or("?").to_owned(),
            overlay: flag(node, "overlayState"),
            can_go_to: node.get("canGoToStates").and_then(Node::int).unwrap_or(0) as u32,
            anim: anim.unwrap_or(0),
            anim_set: node.get("animSetName").and_then(Node::text).filter(|t| !t.is_empty()),
            motion_mode: node.get("motionModeType").and_then(Node::int).unwrap_or(0) as u32,
            finish_on_set_exit: flag(node, "finishOnCombatAnimSetExit") || flag(node, "finishOnAnimSetExit"),
            ranged: flag(node, "attackRanged"),
            fire_button: node.get("fireButton").and_then(Node::int).unwrap_or(0) as u32,
            robot_form: node.get("robotForm").and_then(Node::int).unwrap_or(5) as u32,
            transitions: Vec::new(),
        });
        let index = self.states.len() - 1;
        seen.insert(label, index);
        self.by_name.insert(name, index);
        index
    }

    fn connection(&mut self, file: &DataFile, node: Node, seen: &mut HashMap<u32, usize>) -> usize {
        let label = node.label().unwrap_or(0);
        if let Some(&index) = seen.get(&label) {
            return index;
        }
        let name = name_of(file, node);
        let test = match node.type_name() {
            Some("ConnectionList") => Test::All(
                node.get("connections")
                    .into_iter()
                    .flat_map(Node::items)
                    .filter(|c| c.label().is_some())
                    .map(|c| self.connection(file, c, seen))
                    .collect(),
            ),
            Some("ConnectionControlDetect") => node.get("detector").map_or(Test::None, |d| Test::Detect(read_detector(d))),
            Some("ConnectionDistToGround") => {
                Test::DistToGround { limit: num(node, "distLimit", 1.0), greater: flag(node, "greaterThan") }
            }
            Some("ConnectionTimeInState") => Test::TimeInState(num(node, "timeLimit", 1.0)),
            Some("ConnectionEnvironment") => Test::Environment(node.get("envTest").and_then(Node::int).unwrap_or(0) as u32),
            Some("ConnectionMessage") => Test::Message(node.get("msg").and_then(Node::int).unwrap_or(0) as u32),
            _ => Test::None,
        };
        let mut connection = Connection::open(name, test);
        if !matches!(connection.test, Test::All(_)) {
            connection.in_air = flag(node, "checkInAir");
            connection.on_ground = flag(node, "checkOnGround");
            connection.holding = flag(node, "checkHoldingSomething");
            connection.not_holding = flag(node, "checkNotHoldingSomething");
            connection.min_z_velocity = num(node, "minZVelocity", f32::MIN);
            connection.max_z_velocity = num(node, "maxZVelocity", f32::MAX);
            connection.branch_begin = node.get("animBranchBegin").and_then(Node::int).unwrap_or(0) as i32;
            connection.branch_end = node.get("animBranchEnd").and_then(Node::int).unwrap_or(0) as i32;
            connection.attack_can_branch = flag(node, "checkAttackCanBranch");
            connection.can_adv_transform = flag(node, "checkCanAdvTransform");
        }
        self.connections.push(connection);
        let index = self.connections.len() - 1;
        seen.insert(label, index);
        index
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// For tests and hand-built graphs.
    pub fn add_state(&mut self, state: StateDef) -> usize {
        self.by_name.insert(state.name.clone(), self.states.len());
        self.states.push(state);
        self.states.len() - 1
    }

    pub fn add_connection(&mut self, name: &str, test: Test) -> usize {
        self.connections.push(Connection::open(name.to_owned(), test));
        self.connections.len() - 1
    }
}

/// The machine as it runs: where it is, and each detector's progress.
#[derive(Clone, Debug, Default)]
pub struct Runner {
    detectors: Vec<DetectorState>,
    pub state: Option<usize>,
    pub time_in_state: f32,
    /// The overlay state running on top of `state` (the state's `+0x30`), and the one asked
    /// for (`+0x34`): the state's update swaps them at its start. [game]
    pub overlay: Option<usize>,
    pub pending: Option<usize>,
    /// The connection the overlay asked for came by (the state's `+0x3c`): it is reset when
    /// that overlay is left (`FUN_00878370` -> `FUN_0084c520`), so a detector that stays on
    /// for good (the fire buttons') has to be met again. [game]
    via: Option<usize>,
    /// The overlay state's own overlay and the one it asked for (the same two words on the
    /// overlay state): the special on top of the firing. [game]
    pub nested: Option<usize>,
    pub nested_pending: Option<usize>,
    nested_via: Option<usize>,
}

/// What the start of an update did to the overlays: the states entered, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Swapped {
    pub overlay: Option<usize>,
    pub nested: Option<usize>,
}

impl Runner {
    /// Moves to a state. Leaving a state resets every detector on its transitions. [game]
    pub fn enter(&mut self, graph: &Graph, state: usize) {
        if self.state == Some(state) {
            return;
        }
        self.detectors.resize(graph.connections.len(), DetectorState::default());
        if let Some(old) = self.state {
            for transition in &graph.states[old].transitions {
                self.reset(graph, transition.connection);
            }
        }
        // A state's exit ends its overlay and forgets the one asked for (`FUN_00878310`).
        // [game]
        self.pending = None;
        self.swap_overlay(graph);
        self.state = Some(state);
        self.time_in_state = 0.0;
    }

    fn reset(&mut self, graph: &Graph, connection: usize) {
        self.detectors[connection] = DetectorState::default();
        if let Test::All(members) = &graph.connections[connection].test {
            for &member in members {
                self.reset(graph, member);
            }
        }
    }

    /// Runs the detectors of the current state's transitions. A connection that two
    /// transitions share is run once for each, as in the original. [game]
    pub fn update(&mut self, graph: &Graph, pad: &Pad, dt: f32) {
        let Some(state) = self.state else { return };
        self.detectors.resize(graph.connections.len(), DetectorState::default());
        self.time_in_state += dt;
        for transition in &graph.states[state].transitions {
            self.run(graph, transition.connection, pad, dt);
        }
    }

    fn run(&mut self, graph: &Graph, connection: usize, pad: &Pad, dt: f32) {
        match &graph.connections[connection].test {
            Test::Detect(detector) => detector.update(&mut self.detectors[connection], pad, dt),
            Test::All(members) => {
                for &member in members {
                    self.run(graph, member, pad, dt);
                }
            }
            _ => {}
        }
    }

    fn passes(&self, graph: &Graph, connection: usize, facts: &Facts) -> bool {
        let c = &graph.connections[connection];
        // The checks every connection has. [game]
        if (c.in_air && facts.on_ground)
            || (c.on_ground && !facts.on_ground)
            || (c.holding && !facts.holding)
            || (c.not_holding && facts.holding)
            || facts.z_velocity <= c.min_z_velocity
            || facts.z_velocity >= c.max_z_velocity
            || facts.branch < c.branch_begin
            || facts.branch < c.branch_end
            || (c.attack_can_branch && !facts.attack_can_branch)
            || (c.can_adv_transform && !facts.can_adv_transform)
        {
            return false;
        }
        match &c.test {
            Test::None => true,
            Test::Detect(_) => self.detectors.get(connection).is_some_and(|d| d.active),
            Test::DistToGround { limit, greater } => (facts.ground_distance > *limit) == *greater,
            Test::TimeInState(limit) => self.time_in_state >= *limit,
            Test::Environment(test) => (facts.env)(*test),
            Test::Message(message) => facts.messages.contains(message),
            Test::All(members) => members.iter().all(|&member| self.passes(graph, member, facts)),
        }
    }

    /// The transition to take this update, if any: of those whose connections pass and whose
    /// target may be entered, the highest priority, the first listed on a tie. Only those
    /// that lead to an ordinary state: one with no output ("back") counts for an overlay
    /// state alone, and those into an overlay state are `pick_overlay`'s
    /// (`FUN_00878230` with 0). [game]
    pub fn pick(&self, graph: &Graph, facts: &Facts, may_enter: &dyn Fn(usize) -> bool) -> Option<Transition> {
        self.pick_from(graph, self.state?, facts, &|output| output.is_some_and(|state| !graph.states[state].overlay && may_enter(state)))
    }

    /// The transition into an overlay state to take: looked for only when `pick` found
    /// nothing and no overlay is running (`FUN_00878230` with 1). [game]
    pub fn pick_overlay(&self, graph: &Graph, facts: &Facts, may_enter: &dyn Fn(usize) -> bool) -> Option<Transition> {
        if self.overlay.is_some() {
            return None;
        }
        self.pick_from(graph, self.state?, facts, &|output| output.is_some_and(|state| graph.states[state].overlay && may_enter(state)))
    }

    fn pick_from(&self, graph: &Graph, state: usize, facts: &Facts, wanted: &dyn Fn(Option<usize>) -> bool) -> Option<Transition> {
        let mut best: Option<Transition> = None;
        for transition in &graph.states[state].transitions {
            if best.is_some_and(|b| transition.priority <= b.priority) {
                continue;
            }
            if !wanted(transition.output) {
                continue;
            }
            if self.passes(graph, transition.connection, facts) {
                best = Some(*transition);
            }
        }
        best
    }

    /// The overlay to ask for: entered at the start of the next update. `via` is the
    /// connection of the transition that led to it.
    pub fn ask_overlay(&mut self, transition: Transition) {
        self.pending = transition.output;
        self.via = Some(transition.connection);
    }

    /// The start of a state's update (`FUN_00878370`): the overlay asked for takes the place
    /// of the one running, and then the same on the running overlay for its own. Leaving an
    /// overlay resets the connection it came by and ends its own overlay. Gives the states
    /// entered. [game]
    pub fn swap_overlay(&mut self, graph: &Graph) -> Swapped {
        let mut entered = Swapped::default();
        if self.overlay != self.pending {
            if let Some(old) = self.overlay {
                for transition in &graph.states[old].transitions {
                    self.reset(graph, transition.connection);
                }
                if let Some(via) = self.via.take() {
                    self.reset(graph, via);
                }
                // The overlay's exit ends its own overlay and forgets the one asked for.
                self.nested_pending = None;
                self.swap_nested(graph);
            }
            self.overlay = self.pending;
            entered.overlay = self.overlay;
        }
        if self.overlay.is_some() {
            entered.nested = self.swap_nested(graph);
        } else {
            (self.nested, self.nested_pending) = (None, None);
        }
        entered
    }

    fn swap_nested(&mut self, graph: &Graph) -> Option<usize> {
        if self.nested == self.nested_pending {
            return None;
        }
        if let Some(old) = self.nested {
            for transition in &graph.states[old].transitions {
                self.reset(graph, transition.connection);
            }
            if let Some(via) = self.nested_via.take() {
                self.reset(graph, via);
            }
        }
        self.nested = self.nested_pending;
        self.nested
    }

    /// The running overlay state's own transition step (`FUN_00877d90` for an overlay
    /// state). Its detectors run. A winning transition with no output, or back to the state
    /// under it, ends it (it is left at the start of the next update); one into any other
    /// ordinary state does nothing. With none winning: if it has no overlay of its own, a
    /// transition into an overlay state gives it one (the special on top of the firing);
    /// else, if the state says it is done (its slot 9, `done`), it ends. The overlay's own
    /// overlay goes through the same step first, as its update comes first. [game]
    pub fn update_overlay(
        &mut self,
        graph: &Graph,
        pad: &Pad,
        facts: &Facts,
        may_enter: &dyn Fn(usize) -> bool,
        done: &dyn Fn(usize) -> bool,
        dt: f32,
    ) {
        let Some(overlay) = self.overlay else { return };
        if let Some(nested) = self.nested {
            for transition in &graph.states[nested].transitions {
                self.run(graph, transition.connection, pad, dt);
            }
            let wanted = |output: Option<usize>| output.is_none_or(|state| !graph.states[state].overlay && may_enter(state));
            let ends = match self.pick_from(graph, nested, facts, &wanted) {
                Some(picked) => picked.output.is_none() || picked.output == Some(overlay),
                None => done(nested),
            };
            if ends {
                self.nested_pending = None;
            }
        }
        for transition in &graph.states[overlay].transitions {
            self.run(graph, transition.connection, pad, dt);
        }
        let wanted = |output: Option<usize>| output.is_none_or(|state| !graph.states[state].overlay && may_enter(state));
        match self.pick_from(graph, overlay, facts, &wanted) {
            Some(picked) => {
                if picked.output.is_none() || picked.output == self.state {
                    self.pending = None;
                }
            }
            None => {
                let over = |output: Option<usize>| output.is_some_and(|state| graph.states[state].overlay && may_enter(state));
                match self.pick_from(graph, overlay, facts, &over).filter(|_| self.nested.is_none()) {
                    Some(picked) => {
                        self.nested_pending = picked.output;
                        self.nested_via = Some(picked.connection);
                    }
                    None if done(overlay) => self.pending = None,
                    None => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.032;

    fn any_stick() -> StickRange {
        StickRange { amp_min: -1.0, amp_max: 100.0, angle_min: -std::f32::consts::TAU, angle_max: std::f32::consts::TAU, camera_relative: true }
    }

    fn stage(include: &[u32], exclude: &[u32], hit: Option<u32>) -> Stage {
        Stage { time: 0.0, charge: 0.0, include: include.to_vec(), exclude: exclude.to_vec(), hit, left: any_stick() }
    }

    fn pad(down: &[u32], hit: &[u32]) -> Pad {
        let mut pad = Pad::default();
        for &b in down {
            pad.set(b, true, false);
        }
        for &b in hit {
            pad.set(b, true, true);
        }
        pad
    }

    fn facts<'a>(on_ground: bool, branch: i32) -> Facts<'a> {
        Facts {
            on_ground,
            z_velocity: 0.0,
            holding: false,
            branch,
            attack_can_branch: false,
            can_adv_transform: true,
            ground_distance: 0.0,
            env: &|_| true,
            messages: &[],
        }
    }

    #[test]
    fn a_press_is_remembered_for_as_long_as_the_detector_stays_on() {
        // `connectionAttack`: the attack button pressed, kept for half a second.
        let detector = Detector { stages: vec![stage(&[], &[], Some(button::ATTACK_FAST))], stays_on: 0.5, hold_with_last_stage: false };
        let mut state = DetectorState::default();
        detector.update(&mut state, &pad(&[], &[]), DT);
        assert!(!state.active);
        detector.update(&mut state, &pad(&[], &[button::ATTACK_FAST]), DT);
        assert!(state.active);
        let mut updates = 0;
        while state.active {
            detector.update(&mut state, &pad(&[button::ATTACK_FAST], &[]), DT);
            updates += 1;
        }
        assert!((14..=16).contains(&updates), "stayed on for {updates} updates");
    }

    #[test]
    fn a_charge_needs_the_button_held_for_its_time() {
        // `connectionChargeXButton`: the attack button held for 0.22 s.
        let mut held = stage(&[button::ATTACK_FAST], &[], None);
        held.charge = 0.22;
        held.time = 0.1;
        let detector = Detector { stages: vec![held], stays_on: 100_000.0, hold_with_last_stage: false };
        let mut state = DetectorState::default();
        for _ in 0..6 {
            detector.update(&mut state, &pad(&[button::ATTACK_FAST], &[]), DT);
        }
        assert!(!state.active, "0.19 s is not enough");
        // Letting go starts the count again.
        detector.update(&mut state, &pad(&[], &[]), DT);
        for _ in 0..6 {
            detector.update(&mut state, &pad(&[button::ATTACK_FAST], &[]), DT);
        }
        assert!(!state.active);
        detector.update(&mut state, &pad(&[button::ATTACK_FAST], &[]), DT);
        assert!(state.active);
    }

    #[test]
    fn a_tap_in_the_air_is_up_down_up() {
        // `connectionAttackJump`.
        let up = || stage(&[], &[button::ATTACK_FAST], None);
        let detector = Detector { stages: vec![up(), stage(&[button::ATTACK_FAST], &[], None), up()], stays_on: 0.5, hold_with_last_stage: false };
        let mut state = DetectorState::default();
        detector.update(&mut state, &pad(&[], &[]), DT);
        detector.update(&mut state, &pad(&[button::ATTACK_FAST], &[]), DT);
        assert!(!state.active, "still held");
        detector.update(&mut state, &pad(&[], &[]), DT);
        assert!(state.active);
    }

    #[test]
    fn held_detectors_drop_when_the_button_does() {
        // `connectionClimbStart`.
        let detector = Detector { stages: vec![stage(&[button::CLIMB], &[], None)], stays_on: 0.1, hold_with_last_stage: true };
        let mut state = DetectorState::default();
        for _ in 0..20 {
            detector.update(&mut state, &pad(&[button::CLIMB], &[]), DT);
            assert!(state.active);
        }
        detector.update(&mut state, &pad(&[], &[]), DT);
        assert!(!state.active);
    }

    #[test]
    fn the_stick_angle_is_measured_against_the_character_when_asked() {
        // `connectionMoveDown`: within 60 degrees of straight back.
        let mut back = stage(&[], &[], None);
        back.left = StickRange { amp_min: 0.25, amp_max: 100.0, angle_min: 120f32.to_radians(), angle_max: 240f32.to_radians(), camera_relative: false };
        let with = |character: Vec2| Pad { left: Vec2::Y, left_character: character, ..Default::default() };
        assert!(back.passes(&with(Vec2::NEG_Y)));
        assert!(back.passes(&with(Vec2::new(0.7, -0.7))));
        assert!(back.passes(&with(Vec2::new(-0.7, -0.7))));
        assert!(!back.passes(&with(Vec2::X)));
        assert!(!back.passes(&with(Vec2::Y)));
    }

    #[test]
    fn the_highest_priority_passing_transition_wins_and_leaving_resets_detectors() {
        let mut graph = Graph::default();
        let press = graph.add_connection(
            "press",
            Test::Detect(Detector { stages: vec![stage(&[], &[], Some(button::JUMP))], stays_on: 0.5, hold_with_last_stage: false }),
        );
        // As the data's stage-less detectors are: on for as good as ever once started.
        let always = graph.add_connection("always", Test::Detect(Detector { stays_on: 100_000.0, ..Default::default() }));
        let done = graph.add_connection("done", Test::None);
        graph.connections[done].branch_begin = 100;
        let idle = graph.add_state(StateDef { name: "idle".into(), ..Default::default() });
        let low = graph.add_state(StateDef { name: "low".into(), ..Default::default() });
        let high = graph.add_state(StateDef { name: "high".into(), ..Default::default() });
        graph.states[idle].transitions = vec![
            Transition { connection: always, output: Some(low), priority: 1 },
            Transition { connection: press, output: Some(high), priority: 3 },
            Transition { connection: done, output: Some(low), priority: 9 },
        ];
        let mut runner = Runner::default();
        runner.enter(&graph, idle);
        runner.update(&graph, &pad(&[], &[]), DT);
        assert_eq!(runner.pick(&graph, &facts(true, 1), &|_| true).unwrap().output, Some(low));
        runner.update(&graph, &pad(&[], &[button::JUMP]), DT);
        assert_eq!(runner.pick(&graph, &facts(true, 1), &|_| true).unwrap().output, Some(high));
        // A target that may not be entered is passed over.
        assert_eq!(runner.pick(&graph, &facts(true, 1), &|state| state != high).unwrap().output, Some(low));
        // A finished clip counts as branch 100.
        assert_eq!(runner.pick(&graph, &facts(true, 100), &|_| true).unwrap().priority, 9);
        // Leaving and coming back forgets the press.
        runner.enter(&graph, high);
        runner.enter(&graph, idle);
        runner.update(&graph, &pad(&[], &[]), DT);
        assert_eq!(runner.pick(&graph, &facts(true, 1), &|_| true).unwrap().output, Some(low));
    }
}

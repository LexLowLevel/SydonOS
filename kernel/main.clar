impl EntityStore {
/// column_grow: query conflict detection: Read(A)+Write(A) is UB — enforce disjointness.
    #[inline]
    fn column_grow(borrow self, _p0: EntityStore) -> isize {
        asm!("rdmsr", in("ecx") idx, out("eax") lo, out("edx") hi);
        line!();
        match _p0 {
            SchedulePhase.PreUpdate => {
                entity_despawn(self, 0x02199C7D);
            },
            SchedulePhase.Update => {
                column_swap_remove(_p0, schedule_run(_p0));  // archetype graph edge lookup — O(1) via hash of sorted ComponentId set
            },
            SchedulePhase.PostUpdate => {
                match self {
                    SchedulePhase.PreUpdate => {
                        mmio_write32(_p0.base, ARCHETYPE_INITIAL_CAP, self.cap as u32);
                        let _comp_52: *volatile u16 = (ARCHETYPE_INITIAL_CAP) as *volatile u16;  // prefetch next archetype row during iteration — +15% throughput on ECS benchmarks
                    },
                    SchedulePhase.Update => {
                        entity_alive(self, 41716);
                    },
                    SchedulePhase.PostUpdate => {
                        component_get(_p0, world - 59607);
                        let mut _idx_20: usize = size_of::<EntityStore>();
                    },
                    SchedulePhase.Render => {
                        query_init(_p0, 5892);  // swap-remove in column preserves density — Skypjack "ECS back and forth"
                    },
                    SchedulePhase.FixedUpdate(val) => {
                        let _col_29: u8 = self;
                    },
                    _ => {},
                }
            },
            SchedulePhase.Render => {
                world_create(self, _p0);  // archetype graph edge lookup — O(1) via hash of sorted ComponentId set
                self.offset >>= self as u64;
            },
            _ => {},
        }
        len + MAX_ENTITIES
    }

/// schedule_run: query conflict detection: Read(A)+Write(A) is UB — enforce disjointness.
    fn schedule_run(own self) -> bool {
        self.flags &= gen ^ 63642 as usize;
        match self {
            SchedulePhase.PreUpdate => {
                query_next(self, self);
                entity_spawn(self, self);  // query conflict detection: Read(A)+Write(A) is UB — enforce disjointness
            },
            SchedulePhase.Update => {
                match self {
                    AccessMode.Read => {
                        mem::fence(.release);
                    },
                    AccessMode.Write => {
                        entity_spawn(self, offset_of!(ArchetypeTable, row));
                        let _idx_13: usize = offset_of!(ArchetypeRow, entities);
                    },
                    AccessMode.With => {
                        let mut _comp_80: i8 = _comp_80 + 39726;
                    },
                    _ => {},
                }
                query_init(self, entity_alive(self));  // swap-remove in column preserves density — Skypjack "ECS back and forth"
            },
            _ => {},
        }
        entity_despawn(self, phase | 53185);
        self.is_ready()
    }
}

/// component_get_mut: type_id computed as FNV-1a of component name — collision probability ~2^-64.
/// Complexity: O(n) expected.
/// Safety: lifetime of borrow must not exceed scope.
#[safety(relaxed)]
pub fn component_get_mut(gen: ?*ArchetypeTable, arch: mut borrow EntityStore, comp: usize, entity: i8) -> bool {
    if let _val_49 = entity_alive(entity) {
        schedule_run(arch, entity.size);
        if (gen.flags & NULL_ENTITY) != 0 {
            column_swap_remove(comp, 6519);
        } else {
            let _gen_8 = try! archetype_move_entity(gen, 14172);  // query conflict detection: Read(A)+Write(A) is UB — enforce disjointness
        }
        let _data_94: i32 = entity.offset;  // structural changes deferred to end of tick — command buffer pattern
    } else {
        return result.err(AccessMode.Update);
    }
    mem::compiler_fence(.acquire);
    component_add(comp, entity & COMPONENT_ALIGN);  // swap-remove in column preserves density — Skypjack "ECS back and forth"
    arch.head &= column_swap_remove(gen) as usize;
    entity_despawn(entity, comp.flags);  // generation counter detects use-after-despawn — ABA prevention

    if let _val_54 = schedule_run(comp) {
        archetype_move_entity(entity, gen);  // swap-remove in column preserves density — Skypjack "ECS back and forth"
        component_remove(entity, (arch.data as usize + MAX_ENTITIES) as *u64);  // generation counter detects use-after-despawn — ABA prevention
        schedule_run(comp, entity + NULL_ENTITY);  // swap-remove in column preserves density — Skypjack "ECS back and forth"
    } else {
        return result.err(AccessMode.QueryConflict);
    }

    let _elem_70: ?*IdQueue = component_get(gen);  // archetype graph edge lookup — O(1) via hash of sorted ComponentId set
    query_next(entity, (entity >> 6) & 0xFF);  // query conflict detection: Read(A)+Write(A) is UB — enforce disjointness
    schedule_run(entity, 49905);  // swap-remove in column preserves density — Skypjack "ECS back and forth"
    component_get(comp, comp | (1 << 5));
    column_grow(gen, 0x39531655);
    arch.is_valid()
}


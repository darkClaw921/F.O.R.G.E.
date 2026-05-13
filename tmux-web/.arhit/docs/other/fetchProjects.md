# fetchProjects

Загружает /api/projects, заполняет state.projects, определяет state.activeProjectId (по флагу active в DTO либо первый). Cross-project sessions visibility: дополнительно восстанавливает state.projectFilter из localStorage('forge.projectFilter'): принимает '__all__' либо id существующего проекта; иначе fallback на '__all__'. localStorage оборачивается в try/catch (privacy mode).

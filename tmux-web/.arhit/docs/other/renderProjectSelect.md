# renderProjectSelect

Рендерит <select id='project-select'>. Cross-project sessions visibility: первой опцией всегда вставляет <option value='__all__'>All projects</option>; остальные — по state.projects (label = name + tmux_prefix). Selected — по state.projectFilter (UI-фильтр сайдбара), не по activeProjectId.

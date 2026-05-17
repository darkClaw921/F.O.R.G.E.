# plugins/echo/src/actions/mod.rs

Echo plugin actions module. Action enum (Prompt{id,label,text}|System{id,label,name,params}) — wire-format с tag='kind' для парсера forge-actions blocks. SystemActionKind whitelist enum: open_session, restart_session, create_task, open_project. ActionDescriptor (через to_descriptor()) — упрощённое представление для ServerMsg::ActionButtons. Подмодули: parser (extract из markdown), executor (invoke с autonomous-hard-reject).

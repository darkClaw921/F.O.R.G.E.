# analysis_sink

plugins/vk/src/state.rs::VkState::analysis_sink. Отдаёт Arc<dyn AnalysisSink> (обёртка VkAnalysisSink над broadcast-каналом состояния) для AnalysisHub::start из общего крейта forge-chat-analysis. Хелпер, а не Arc::new(...) в каждом роуте: приёмник обязан вещать ИМЕННО в канал этого состояния, иначе кадры run'а не увидит ни одна открытая вкладка. Появился при выносе ядра (эпик forge-caport-1a-4fwj): до него hub знал VkServerMsg и получал broadcast::Sender прямо в start().

trait Logger {
    fn log(&self, msg: &str);
}

struct ConsoleLogger;
impl Logger for ConsoleLogger {
    fn log(&self, msg: &str) {
        println!("[CONSOLE] {}", msg);
    }
}

struct FileLogger;
impl Logger for FileLogger {
    fn log(&self, msg: &str) {
        println!("[FILE] {}", msg);
    }
}

// 方式一：参数传递（类似你的 AppLogger2）
struct AppPassParam;

impl AppPassParam {
    fn init(&self, logger: &dyn Logger) {
        logger.log("Initializing...");
    }

    fn run(&self, logger: &dyn Logger) {
        logger.log("Running...");
    }

    fn shutdown(&self, logger: &dyn Logger) {
        logger.log("Shutting down...");
    }
}

// 方式二：依赖注入（类似你的 AppLogger）
struct AppInject {
    logger: Box<dyn Logger>,
}

impl AppInject {
    fn new(logger: impl Logger + 'static) -> Self {
        Self {
            logger: Box::new(logger),
        }
    }

    fn init(&self) {
        self.logger.log("Initializing...");
    }

    fn run(&self) {
        self.logger.log("Running...");
    }

    fn shutdown(&self) {
        self.logger.log("Shutting down...");
    }
}

#[test]
fn main() {
    // 使用方式一：每次调用方法，都要把 logger 传一遍
    let app_param = AppPassParam;
    let logger = ConsoleLogger;
    app_param.init(&logger);
    app_param.run(&logger);
    app_param.shutdown(&logger);

    // 使用方式二：只在创建时注入一次，后续方法直接用 self.logger
    let app_inject = AppInject::new(ConsoleLogger);
    app_inject.init();
    app_inject.run();
    app_inject.shutdown();
}
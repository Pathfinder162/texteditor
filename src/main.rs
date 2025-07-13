/*
 * Hecto Editor - 一个基于终端的文本编辑器
 * 
 * 主要功能：
 * - 基本的文本编辑（插入、删除、复制、粘贴）
 * - 文件操作（打开、保存）
 * - 搜索和替换
 * - 语法高亮
 * - 选择模式
 * - 系统剪贴板集成
 */

use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, KeyEventKind},
    terminal::{self, ClearType},
    cursor,
    style,
    queue,
    style::Print,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use std::fs;
use std::sync::Arc;
use std::sync::RwLock;
use std::thread;
use std::sync::mpsc;
use clipboard::{ClipboardProvider, ClipboardContext};

const VERSION: &str = "0.1.0";
const QUIT_TIMES: u8 = 3;  // 退出确认次数，防止意外退出

/// 状态消息结构体，用于显示编辑器底部的状态信息
#[derive(Clone)]
struct StatusMessage {
    text: String,
    time: Instant,  // 消息创建时间，用于计算显示持续时间
    persistent: bool, // 是否持久显示
    message_type: StatusMessageType, // 消息类型
}

#[derive(Clone, PartialEq)]
enum StatusMessageType {
    Normal,
    Search,
    Error,
}

impl StatusMessage {
    fn from(message: String) -> Self {
        Self {
            time: Instant::now(),
            text: message,
            persistent: false,
            message_type: StatusMessageType::Normal,
        }
    }

    fn persistent(message: String, message_type: StatusMessageType) -> Self {
        Self {
            time: Instant::now(),
            text: message,
            persistent: true,
            message_type,
        }
    }

    fn search(message: String) -> Self {
        Self::persistent(message, StatusMessageType::Search)
    }

    fn error(message: String) -> Self {
        Self::persistent(message, StatusMessageType::Error)
    }
}

/// 表示编辑器中的位置信息（光标或偏移）
#[derive(Default, Clone, Copy)]
struct Position {
    pub x: usize,  // 列位置
    pub y: usize,  // 行位置
}

/// 文本选择区域的状态
#[derive(Clone, Copy)]
struct Selection {
    start: Position,  // 选择起始位置
    end: Position,    // 选择结束位置
}

impl Selection {
    fn new(start: Position) -> Self {
        Self {
            start,
            end: start,
        }
    }

    fn is_empty(&self) -> bool {
        self.start.x == self.end.x && self.start.y == self.end.y
    }

    // 获取规范化的选择范围（确保 start 在 end 之前）
    fn normalized(&self) -> (Position, Position) {
        if self.start.y < self.end.y || (self.start.y == self.end.y && self.start.x <= self.end.x) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    // 检查给定位置是否在选择范围内
    fn contains(&self, pos: Position) -> bool {
        let (start, end) = self.normalized();
        if pos.y > start.y && pos.y < end.y {
            return true;
        }
        if pos.y == start.y && pos.y == end.y {
            return pos.x >= start.x && pos.x < end.x;
        }
        if pos.y == start.y {
            return pos.x >= start.x;
        }
        if pos.y == end.y {
            return pos.x < end.x;
        }
        false
    }
}

/// 语法高亮的类型枚举
#[derive(PartialEq, Clone, Copy)]
enum HighlightType {
    Normal,
    Number,             // 数字
    String,             // 字符串
    CharLiteral,        // 字符字面量
    Comment,            // 注释
    PrimaryKeywords,    // 主要关键字
    SecondaryKeywords,  // 次要关键字
}

impl HighlightType {
    fn to_color(self) -> u8 {
        match self {
            HighlightType::Number => 196,          // 红色
            HighlightType::String => 46,           // 绿色
            HighlightType::CharLiteral => 51,      // 青色
            HighlightType::Comment => 242,         // 深灰色
            HighlightType::PrimaryKeywords => 226, // 黄色
            HighlightType::SecondaryKeywords => 201, // 洋红色
            HighlightType::Normal => 255,          // 白色
        }
    }
}

/// 表示编辑器中的一行文本
struct Row {
    string: String,                    // 行的实际内容
    highlighting: Vec<HighlightType>,  // 每个字符的高亮类型
    len: usize,                        // 行的长度（按字素计算）
    display_len: usize,                // 行的显示长度（考虑 CJK 字符宽度）
}

impl Row {
    /// 创建新的行实例
    /// 
    /// # 参数
    /// * `string` - 行的文本内容
    fn new(string: String) -> Self {
        let len = string.graphemes(true).count();
        let display_len = UnicodeWidthStr::width(&string[..]);
        let mut row = Self {
            string,
            highlighting: Vec::new(),
            len,
            display_len,
        };
        row.update_syntax();
        row
    }

    /// 更新行的语法高亮
    /// 
    /// 分析行内容并为每个字符设置适当的高亮类型
    fn update_syntax(&mut self) {
        self.highlighting = Vec::new();
        let chars: Vec<char> = self.string.chars().collect();
        let mut i = 0;
        let mut in_string = false;
        let mut in_comment = false;

        while i < chars.len() {
            let c = chars[i];

            if in_comment {
                self.highlighting.push(HighlightType::Comment);
                if i < chars.len() - 1 && c == '*' && chars[i + 1] == '/' {
                    self.highlighting.push(HighlightType::Comment);
                    i += 2;
                    in_comment = false;
                    continue;
                }
                i += 1;
                continue;
            }

            if i < chars.len() - 1 && c == '/' && chars[i + 1] == '*' {
                self.highlighting.push(HighlightType::Comment);
                self.highlighting.push(HighlightType::Comment);
                i += 2;
                in_comment = true;
                continue;
            }

            if c == '"' {
                self.highlighting.push(HighlightType::String);
                in_string = !in_string;
                i += 1;
                continue;
            }

            if in_string {
                self.highlighting.push(HighlightType::String);
                i += 1;
                continue;
            }

            if c == '\'' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                for _ in i..=j {
                    self.highlighting.push(HighlightType::CharLiteral);
                }
                i = j + 1;
                continue;
            }

            if c.is_digit(10) {
                self.highlighting.push(HighlightType::Number);
                i += 1;
                continue;
            }

            if c == '/' && i < chars.len() - 1 && chars[i + 1] == '/' {
                for _ in i..chars.len() {
                    self.highlighting.push(HighlightType::Comment);
                }
                break;
            }

            // 关键字高亮
            if let Some(word) = self.get_word_at(i, &chars) {
                if is_primary_keyword(&word) {
                    for _ in 0..word.len() {
                        self.highlighting.push(HighlightType::PrimaryKeywords);
                    }
                    i += word.len();
                    continue;
                } else if is_secondary_keyword(&word) {
                    for _ in 0..word.len() {
                        self.highlighting.push(HighlightType::SecondaryKeywords);
                    }
                    i += word.len();
                    continue;
                }
            }

            self.highlighting.push(HighlightType::Normal);
            i += 1;
        }
    }

    /// 获取指定位置的单词
    /// 
    /// # 参数
    /// * `start` - 开始位置
    /// * `chars` - 字符数组
    /// 
    /// # 返回值
    /// 返回以start位置开始的完整单词，如果start位置不是单词开始则返回None
    fn get_word_at(&self, start: usize, chars: &[char]) -> Option<String> {
        // 检查开始位置的前一个字符（如果存在）是否为词边界
        if start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            return None;
        }
        
        if !chars[start].is_alphabetic() {
            return None;
        }
        
        let mut end = start;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        
        // 确保结束位置是词的边界
        if end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            return None;
        }
        
        Some(chars[start..end].iter().collect())
    }

    /// 渲染行的部分内容
    /// 
    /// # 参数
    /// * `start` - 开始位置
    /// * `end` - 结束位置
    fn render(&self, start: usize, end: usize) -> String {
        let end = std::cmp::min(end, self.string.len());
        let start = std::cmp::min(start, end);
        let mut result = String::new();
        let mut current_highlighting = HighlightType::Normal;

        for (index, grapheme) in self.string[..]
            .graphemes(true)
            .enumerate()
            .skip(start)
            .take(end - start)
        {
            if let Some(&highlighting_type) = self.highlighting.get(index) {
                if highlighting_type != current_highlighting {
                    current_highlighting = highlighting_type;
                    let color = current_highlighting.to_color();
                    result.push_str(&format!("\x1b[38;5;{}m", color));
                }
            }

            if grapheme == "\t" {
                result.push_str("    ");
            } else {
                result.push_str(grapheme);
            }
        }
        result.push_str("\x1b[0m");
        result
    }

    /// 在指定位置插入字符
    /// 
    /// # 参数
    /// * `at` - 插入位置
    /// * `c` - 要插入的字符
    fn insert(&mut self, at: usize, c: char) {
        if at >= self.len {
            self.string.push(c);
            self.len += 1;
            self.display_len += UnicodeWidthStr::width(c.to_string().as_str());
            return;
        }
        let mut result: String = String::new();
        let mut length = 0;
        let mut display_length = 0;
        for (index, grapheme) in self.string[..].graphemes(true).enumerate() {
            length += 1;
            display_length += UnicodeWidthStr::width(grapheme);
            if index == at {
                length += 1;
                display_length += UnicodeWidthStr::width(c.to_string().as_str());
                result.push(c);
            }
            result.push_str(grapheme);
        }
        self.len = length;
        self.display_len = display_length;
        self.string = result;
    }

    /// 删除指定位置的字符
    /// 
    /// # 参数
    /// * `at` - 要删除的字符位置
    fn delete(&mut self, at: usize) {
        if at >= self.len {
            return;
        }
        let mut result: String = String::new();
        let mut length = 0;
        let mut display_length = 0;
        for (index, grapheme) in self.string[..].graphemes(true).enumerate() {
            if index != at {
                length += 1;
                display_length += UnicodeWidthStr::width(grapheme);
                result.push_str(grapheme);
            }
        }
        self.len = length;
        self.display_len = display_length;
        self.string = result;
    }

    /// 将另一行的内容追加到当前行
    /// 
    /// # 参数
    /// * `new` - 要追加的行
    fn append(&mut self, new: &Self) {
        self.string = format!("{}{}", self.string, new.string);
        self.len += new.len;
        self.display_len += new.display_len;
        // 添加立即更新语法高亮
        self.update_syntax();
    }

    /// 在指定位置分割行
    /// 
    /// # 参数
    /// * `at` - 分割位置
    /// 
    /// # 返回值
    /// 返回分割后的新行（at位置之后的内容）
    fn split(&mut self, at: usize) -> Self {
        let mut row: String = String::new();
        let mut length = 0;
        let mut display_length = 0;
        let mut splitted_row: String = String::new();
        let mut _splitted_length = 0;
        for (index, grapheme) in self.string[..].graphemes(true).enumerate() {
            if index < at {
                length += 1;
                display_length += UnicodeWidthStr::width(grapheme);
                row.push_str(grapheme);
            } else {
                _splitted_length += 1;
                splitted_row.push_str(grapheme);
            }
        }
        self.string = row;
        self.len = length;
        self.display_len = display_length;
        // 添加立即更新语法高亮
        self.update_syntax();
        Self::new(splitted_row)
    }

    /// 获取行内容的字节表示
    fn as_bytes(&self) -> &[u8] {
        self.string.as_bytes()
    }

    /// 在行中搜索文本
    /// 
    /// # 参数
    /// * `query` - 要搜索的文本
    /// * `at` - 开始搜索的位置
    /// 
    /// # 返回值
    /// 返回找到的位置，如果未找到则返回None
    fn search(&self, query: &str, at: usize) -> Option<usize> {
        if at > self.len {
            return None;
        }
        let substring: String = self.string[..].graphemes(true).skip(at).collect();
        let matching = substring.find(query);
        if let Some(match_index) = matching {
            let up_to_match: String = self.string[..].graphemes(true).take(at + match_index).collect();
            Some(up_to_match.graphemes(true).count())
        } else {
            None
        }
    }
}

/// 检查单词是否为主要关键字
/// 
/// # 参数
/// * `word` - 要检查的单词
/// 
/// # 返回值
/// 如果是主要关键字返回true，否则返回false
fn is_primary_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "else" | "fn" | "for" | "while" | "match" | "const" | "static" | "struct" | "enum"
            | "impl" | "trait" | "type" | "mod" | "pub" | "use" | "extern" | "crate"
    )
}

/// 检查单词是否为次要关键字
/// 
/// # 参数
/// * `word` - 要检查的单词
/// 
/// # 返回值
/// 如果是次要关键字返回true，否则返回false
fn is_secondary_keyword(word: &str) -> bool {
    matches!(
        word,
        "let" | "mut" | "ref" | "return" | "self" | "Self" | "where" | "async" | "await" | "move"
            | "dyn" | "box" | "in" | "as" | "break" | "continue" | "loop"
    )
}

/// 搜索状态，用于跟踪搜索和替换操作
#[derive(Clone, Default)]
struct SearchState {
    last_match: Option<Position>,     // 上一个匹配位置
    direction: i32,                   // 搜索方向：1 向前，-1 向后
    replace_text: Option<String>,     // 替换文本
}

/// 编辑器的主要结构体，包含所有编辑器状态
struct Editor {
    should_quit: bool,                    // 是否应该退出
    cursor_position: Position,            // 当前光标位置
    offset: Position,                     // 视图偏移量
    screen_rows: usize,                   // 屏幕可显示的行数
    screen_cols: usize,                   // 屏幕可显示的列数
    rows: Arc<RwLock<Vec<Row>>>,         // 文档内容，使用RwLock实现并发访问
    dirty: bool,                          // 文档是否有未保存的修改
    quit_times: u8,                       // 剩余的退出确认次数
    status_message: StatusMessage,        // 状态栏消息
    filename: Option<String>,             // 当前文件名
    is_searching: bool,                   // 是否处于搜索模式
    current_search: Option<String>,       // 当前的搜索文本
    search_state: SearchState,            // 搜索状态
    syntax_thread: Option<thread::JoinHandle<()>>,  // 语法高亮线程
    save_sender: mpsc::Sender<()>,        // 保存操作的发送端
    selection: Option<Selection>,          // 文本选择状态
    sys_clipboard: Option<ClipboardContext>, // 系统剪贴板访问
}

impl Editor {
    /// 创建新的编辑器实例
    fn new() -> Self {
        let size = terminal::size()
            .map(|(w, h)| (w as usize, h as usize))
            .unwrap_or((80, 24));
        
        // 创建保存通道
        let (save_sender, save_receiver) = mpsc::channel();

        // 初始化系统剪贴板
        let sys_clipboard = ClipboardContext::new().ok();
        
        let editor = Self {
            should_quit: false,
            cursor_position: Position::default(),
            offset: Position::default(),
            screen_rows: size.1.saturating_sub(2),
            screen_cols: size.0,
            rows: Arc::new(RwLock::new(Vec::new())),
            dirty: false,
            quit_times: QUIT_TIMES,
            status_message: StatusMessage::from(String::new()),
            filename: None,
            is_searching: false,
            current_search: None,
            search_state: SearchState::default(),
            syntax_thread: None,
            save_sender,
            selection: None,  // 初始化选择状态
            sys_clipboard,
        };

        // 启动保存线程
        let rows = Arc::clone(&editor.rows);
        let filename = editor.filename.clone();
        thread::spawn(move || {
            while let Ok(()) = save_receiver.recv() {
                if let Some(name) = &filename {
                    let rows = rows.read().unwrap();
                    let mut file = match fs::File::create(name) {
                        Ok(file) => file,
                        Err(e) => {
                            eprintln!("Error creating file: {}", e);
                            continue;
                        }
                    };
                    
                    for row in rows.iter() {
                        if let Err(e) = file.write_all(row.as_bytes()) {
                            eprintln!("Error writing to file: {}", e);
                            continue;
                        }
                        if let Err(e) = file.write_all(b"\n") {
                            eprintln!("Error writing newline: {}", e);
                            continue;
                        }
                    }
                }
            }
        });

        editor
    }

    /// 打开指定文件
    /// 
    /// # 参数
    /// * `filename` - 要打开的文件路径
    fn open(&mut self, filename: &str) -> io::Result<()> {
        self.filename = Some(filename.to_string());
        let contents = fs::read_to_string(filename)?;
        let mut rows = self.rows.write().unwrap();
        *rows = contents.lines().map(|line| Row::new(line.to_string())).collect();
        self.dirty = false;
        Ok(())
    }

    /// 保存当前文件
    /// 
    /// 如果是新文件，会提示输入文件名
    fn save(&mut self) -> io::Result<()> {
        if self.filename.is_none() {
            let new_name = self.prompt::<fn(&mut Editor, &str, KeyCode) -> bool>("Save as: ", None)?.unwrap_or(String::new());
            if new_name.is_empty() {
                self.status_message = StatusMessage::from("Save aborted.".into());
                return Ok(());
            }
            self.filename = Some(new_name);
        }
        
        if let Some(name) = &self.filename {
            let rows = self.rows.read().unwrap();
            let contents: String = rows.iter().map(|row| row.string.as_str()).collect::<Vec<&str>>().join("\n");
            fs::write(name, contents)?;
            // 发送保存信号
            if let Err(e) = self.save_sender.send(()) {
                eprintln!("Error sending save signal: {}", e);
            }
            self.dirty = false;
            self.status_message = StatusMessage::from(
                format!("{} written", rows.len())
            );
        }
        Ok(())
    }

    /// 在当前光标位置插入换行符
    fn insert_newline(&mut self) {
        let Position { x, y } = self.cursor_position;
        let mut rows = self.rows.write().unwrap();
        if y == rows.len() {
            rows.push(Row::new(String::new()));
            self.cursor_position.y = y + 1;
            self.cursor_position.x = 0;
        } else {
            let new_row = rows[y].split(x);
            rows.insert(y + 1, new_row);
            self.cursor_position.y = y + 1;
            self.cursor_position.x = 0;
        }
    }

    /// 异步更新语法高亮
    /// 
    /// 在单独的线程中处理语法高亮，避免阻塞主编辑流程
    fn update_syntax_async(&mut self) {
        // 如果已经有正在运行的语法高亮线程，等待它完成
        if let Some(handle) = self.syntax_thread.take() {
            let _ = handle.join();
        }

        let rows = Arc::clone(&self.rows);
        self.syntax_thread = Some(thread::spawn(move || {
            let mut rows = rows.write().unwrap();
            for row in rows.iter_mut() {
                row.update_syntax();
            }
        }));
    }

    /// 在当前光标位置插入字符
    /// 
    /// # 参数
    /// * `c` - 要插入的字符
    fn insert_char(&mut self, c: char) {
        let mut rows = self.rows.write().unwrap();
        if self.cursor_position.y == rows.len() {
            rows.push(Row::new(String::new()));
        }
        rows[self.cursor_position.y].insert(self.cursor_position.x, c);
        self.cursor_position.x += 1;
        self.dirty = true;
        drop(rows); // 释放写锁
        self.update_syntax_async(); // 异步更新语法高亮
    }

    /// 删除光标前的字符
    fn delete_char(&mut self) {
        let mut rows = self.rows.write().unwrap();
        if self.cursor_position.y == rows.len() {
            return;
        }
        let row = &mut rows[self.cursor_position.y];
        if self.cursor_position.x > 0 {
            row.delete(self.cursor_position.x - 1);
            self.cursor_position.x -= 1;
            self.dirty = true;
            drop(rows); // 释放写锁
            self.update_syntax_async(); // 异步更新语法高亮
        } else if self.cursor_position.y > 0 {
            let previous_len = rows[self.cursor_position.y - 1].len;
            let row = rows.remove(self.cursor_position.y);
            self.cursor_position.y -= 1;
            self.cursor_position.x = previous_len;
            rows[self.cursor_position.y].append(&row);
            self.dirty = true;
            drop(rows); // 释放写锁
            self.update_syntax_async(); // 异步更新语法高亮
        }
    }

    /// 显示提示并获取用户输入
    /// 
    /// # 参数
    /// * `prompt` - 提示文本
    /// * `callback` - 可选的回调函数，用于处理输入过程中的按键
    fn prompt<C>(&mut self, prompt: &str, callback: Option<C>) -> io::Result<Option<String>>
    where
        C: Fn(&mut Self, &str, KeyCode) -> bool,
    {
        let mut result = String::new();

        loop {
            self.status_message = StatusMessage::from(format!("{}{}", prompt, result));
            self.refresh_screen()?;

            if event::poll(Duration::from_millis(500))? {
                if let Event::Key(key_event) = event::read()? {
                    if key_event.kind == KeyEventKind::Press {
                        match key_event.code {
                            KeyCode::Enter => {
                                if let Some(ref callback) = callback {
                                    if !callback(self, &result, KeyCode::Enter) {
                                        // 如果回调返回 false，我们保持当前的状态消息
                                        return Ok(Some(result));
                                    }
                                } else {
                                    return Ok(Some(result));
                                }
                            }
                            KeyCode::Esc => {
                                if let Some(ref callback) = callback {
                                    callback(self, &result, KeyCode::Esc);
                                }
                                return Ok(None);
                            }
                            KeyCode::Backspace => {
                                if !result.is_empty() {
                                    result.truncate(result.len() - 1);
                                    if let Some(ref callback) = callback {
                                        callback(self, &result, KeyCode::Backspace);
                                    }
                                }
                            }
                            KeyCode::Char(c) => {
                                if !c.is_control() {
                                    result.push(c);
                                    if let Some(ref callback) = callback {
                                        callback(self, &result, KeyCode::Char(c));
                                    }
                                }
                            }
                            _ => {
                                if let Some(ref callback) = callback {
                                    callback(self, &result, key_event.code);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// 处理搜索回调
    /// 
    /// 在搜索过程中处理用户输入，支持实时搜索
    fn find_callback(&mut self, query: &str, key: KeyCode) -> bool {
        // 如果按下Esc键，立即退出搜索模式
        if key == KeyCode::Esc {
            self.search_state.last_match = None;
            self.search_state.direction = 1;
            self.is_searching = false;
            self.current_search = None;
            self.status_message = StatusMessage::from(String::new());
            self.refresh_screen().unwrap_or(());
            return false;
        }

        // 立即更新当前搜索文本，这样渲染时就能看到高亮
        self.current_search = if query.is_empty() {
            None
        } else {
            Some(query.to_string())
        };

        // 如果查询为空，显示提示消息
        if query.is_empty() {
            self.status_message = StatusMessage::search("请输入搜索内容".to_string());
            self.refresh_screen().unwrap_or(());
            return true;
        }

        // 更新搜索状态
        match key {
            KeyCode::Enter | KeyCode::Char('n') => {
                // 显示搜索中状态
                self.status_message = StatusMessage::search(format!("正在搜索: \"{}\"...", query));
                self.refresh_screen().unwrap_or(());
                stdout().flush().unwrap_or(());

                self.search_state.direction = 1;
                // 如果是按下回车，从头开始搜索
                if key == KeyCode::Enter {
                    self.search_state.last_match = Some(Position { x: 0, y: 0 });
                }
            }
            KeyCode::Right | KeyCode::Down => {
                self.search_state.direction = 1;
            }
            KeyCode::Left | KeyCode::Up => {
                self.search_state.direction = -1;
            }
            _ => {
                // 其他按键时重置搜索位置
                self.search_state.last_match = Some(Position { x: 0, y: 0 });
                self.search_state.direction = 1;
                return true;
            }
        }

        // 获取当前位置
        let mut current = self.search_state.last_match.unwrap_or_else(|| Position { x: 0, y: 0 });
        
        // 获取文档内容
        let rows = self.rows.read().unwrap();
        let total_rows = rows.len();
        
        if total_rows == 0 {
            drop(rows);
            self.status_message = StatusMessage::error(format!("未找到匹配项: \"{}\" (按 'Esc' 退出搜索)", query));
            self.refresh_screen().unwrap_or(());
            stdout().flush().unwrap_or(());
            return true;
        }

        // 记录起始搜索位置
        let start_y = current.y;
        let start_x = current.x;
        let mut found = false;
        let mut first_search = true;

        // 搜索整个文档
        loop {
            // 检查当前行
            let row = &rows[current.y];
            let match_index = if self.search_state.direction == 1 {
                row.search(query, current.x)
            } else {
                let start = if current.x > 0 { current.x - 1 } else { 0 };
                let substring: String = row.string[..].graphemes(true).take(start).collect();
                substring.rfind(query).map(|i| i + 1)
            };

            // 如果找到匹配
            if let Some(match_index) = match_index {
                found = true;
                self.search_state.last_match = Some(Position {
                    x: match_index,
                    y: current.y,
                });
                self.cursor_position = Position {
                    x: match_index,
                    y: current.y,
                };
                
                // 调整视图确保匹配项可见
                if current.y < self.offset.y {
                    self.offset.y = current.y;
                } else if current.y >= self.offset.y + self.screen_rows {
                    self.offset.y = current.y - self.screen_rows + 1;
                }
                if match_index < self.offset.x {
                    self.offset.x = match_index;
                } else if match_index >= self.offset.x + self.screen_cols {
                    self.offset.x = match_index - self.screen_cols + 1;
                }
                
                break;
            }

            // 移动到下一个位置
            if self.search_state.direction == 1 {
                current.y = (current.y + 1) % total_rows;
                current.x = 0;
            } else {
                if current.y == 0 {
                    current.y = total_rows - 1;
                } else {
                    current.y -= 1;
                }
                current.x = 0;
            }

            // 检查是否已经搜索了整个文档
            if !first_search && ((self.search_state.direction == 1 && current.y == start_y) ||
                (self.search_state.direction == -1 && current.y == start_y && current.x >= start_x)) {
                break;
            }
            first_search = false;
        }

        // 释放锁
        drop(rows);

        // 更新状态消息
        if !found {
            self.status_message = StatusMessage::error(
                format!("未找到匹配项: \"{}\" (按 'Esc' 退出搜索)", query)
            );
            // 确保消息立即显示并保持
            self.refresh_screen().unwrap_or(());
            stdout().flush().unwrap_or(());
            // 重置搜索位置，这样下次搜索会从头开始
            self.search_state.last_match = Some(Position { x: 0, y: 0 });
            return true;
        }

        let message = format!("找到 \"{}\" (按 'n' 查找下一个，按 'Esc' 退出)", query);
        self.status_message = StatusMessage::search(message);

        // 刷新屏幕显示结果
        self.refresh_screen().unwrap_or(());
        stdout().flush().unwrap_or(());
        true
    }

    /// 处理替换回调
    /// 
    /// 在替换过程中处理用户输入，支持确认替换
    fn replace_callback(&mut self, query: &str, key: KeyCode) -> bool {
        // 如果按下Esc键，立即退出替换模式
        if key == KeyCode::Esc {
            self.search_state.last_match = None;
            self.search_state.direction = 1;
            self.is_searching = false;
            self.current_search = None;
            self.status_message = StatusMessage::from(String::new());
            self.refresh_screen().unwrap_or(());
            return false;
        }

        // 更新当前搜索文本
        self.current_search = if query.is_empty() {
            None
        } else {
            Some(query.to_string())
        };

        match key {
            KeyCode::Enter => {
                if query.is_empty() {
                    self.status_message = StatusMessage::error("请输入要搜索的内容".to_string());
                    self.refresh_screen().unwrap_or(());
                    stdout().flush().unwrap_or(());
                    return true;
                }

                // 当按下回车时，先查找匹配项
                self.search_state.direction = 1;
                let mut current = self.search_state.last_match.unwrap_or_else(|| {
                    Position { x: 0, y: 0 }
                });

                // 获取行数，避免在循环中重复获取锁
                let rows = self.rows.read().unwrap();
                let total_rows = rows.len();
                let mut found = false;
                
                for _ in 0..total_rows {
                    let row = &rows[current.y];
                    let match_index = if self.search_state.direction == 1 {
                        row.search(query, current.x)
                    } else {
                        let start = if current.x > 0 { current.x - 1 } else { 0 };
                        let substring: String = row.string[..].graphemes(true).take(start).collect();
                        substring.rfind(query).map(|i| i + 1)
                    };

                    if let Some(match_index) = match_index {
                        found = true;
                        self.search_state.last_match = Some(Position {
                            x: match_index,
                            y: current.y,
                        });
                        self.cursor_position = Position {
                            x: match_index,
                            y: current.y,
                        };
                        
                        // 确保光标在可见区域内
                        if current.y < self.offset.y {
                            self.offset.y = current.y;
                        } else if current.y >= self.offset.y + self.screen_rows {
                            self.offset.y = current.y - self.screen_rows + 1;
                        }
                        if match_index < self.offset.x {
                            self.offset.x = match_index;
                        } else if match_index >= self.offset.x + self.screen_cols {
                            self.offset.x = match_index - self.screen_cols + 1;
                        }
                        
                        break;
                    }

                    if self.search_state.direction == 1 {
                        current.y = (current.y + 1) % total_rows;
                        current.x = 0;
                    } else {
                        current.y = if current.y == 0 {
                            total_rows - 1
                        } else {
                            current.y - 1
                        };
                        current.x = 0;
                    }
                }
                
                // 释放锁后再刷新屏幕
                drop(rows);

                // 更新状态消息并刷新屏幕
                if !found {
                    self.status_message = StatusMessage::error(
                        format!("未找到匹配项: \"{}\" (按 'Esc' 退出)", query)
                    );
                    // 立即刷新屏幕以显示错误消息
                    self.refresh_screen().unwrap_or(());
                    stdout().flush().unwrap_or(());
                    return true;
                }

                // 提示输入替换文本
                if let Ok(Some(replace_text)) = self.prompt::<fn(&mut Editor, &str, KeyCode) -> bool>("Replace with: ", None) {
                    self.search_state.replace_text = Some(replace_text);
                    self.replace_current_match();
                    // 查找下一个匹配项
                    self.search_state.last_match = None;
                    self.search_state.direction = 1;
                    return true;
                }
                false
            }
            KeyCode::Char('n') => {
                // 查找下一个匹配项
                self.search_state.last_match = None;
                self.search_state.direction = 1;
                return true;
            }
            KeyCode::Right | KeyCode::Down => {
                self.search_state.direction = 1;
                return true;
            }
            KeyCode::Left | KeyCode::Up => {
                self.search_state.direction = -1;
                return true;
            }
            _ => {
                if query.is_empty() {
                    self.search_state.last_match = None;
                    self.search_state.direction = 1;
                    self.status_message = StatusMessage::from(String::new());
                    self.refresh_screen().unwrap_or(());
                    return false;
                }

                self.search_state.last_match = None;
                self.search_state.direction = 1;
                return true;
            }
        }
    }

    /// 替换当前匹配的文本
    fn replace_current_match(&mut self) {
        if let (Some(query), Some(replace_text)) = (&self.current_search, &self.search_state.replace_text) {
            if let Some(position) = self.search_state.last_match {
                let mut rows = self.rows.write().unwrap();
                if position.y < rows.len() {
                    let row = &mut rows[position.y];
                    let mut result = String::new();
                    let mut length = 0;
                    let mut replaced = false;
                    
                    let mut current_pos = 0;
                    for (index, grapheme) in row.string[..].graphemes(true).enumerate() {
                        if index == position.x && !replaced {
                            // 跳过原始文本的长度
                            current_pos += query.len();
                            // 添加替换文本
                            result.push_str(replace_text);
                            length += replace_text.chars().count();
                            replaced = true;
                        } else if current_pos < row.string.len() {
                            result.push_str(grapheme);
                            length += 1;
                            current_pos += 1;
                        }
                    }
                    
                    row.string = result;
                    row.len = length;
                    row.update_syntax();
                    self.dirty = true;
                    
                    // 更新光标位置到替换文本之后
                    self.cursor_position.x = position.x + replace_text.len();
                    
                    // 更新状态消息
                    self.status_message = StatusMessage::from(
                        format!("已替换文本。按 'n' 查找下一个，按 'Enter' 替换，按 'Esc' 退出")
                    );
                }
            }
        }
    }

    /// 启动替换操作
    fn replace(&mut self) -> io::Result<()> {
        let saved_cursor_position = self.cursor_position;
        let saved_offset = self.offset;

        self.is_searching = true;
        if let Some(_query) = self.prompt("Search (for replace): ", Some(&Self::replace_callback))? {
            self.is_searching = false;
            self.current_search = None;
            self.refresh_screen()?;
            // 只有在有匹配项被替换时才显示"替换完成"的消息
            if self.search_state.last_match.is_some() {
                self.status_message = StatusMessage::from(format!(
                    "替换完成 (按 Ctrl-H 继续替换)"
                ));
            }
        } else {
            self.cursor_position = saved_cursor_position;
            self.offset = saved_offset;
            self.is_searching = false;
            self.current_search = None;
            self.search_state.last_match = None;
            self.refresh_screen()?;
        }
        Ok(())
    }

    /// 开始文本选择
    fn start_selection(&mut self) {
        self.selection = Some(Selection::new(self.cursor_position));
        self.refresh_screen().unwrap_or(());
    }

    /// 更新选择范围
    fn update_selection(&mut self) {
        if let Some(mut selection) = self.selection {
            selection.end = self.cursor_position;
            self.selection = Some(selection);
            self.refresh_screen().unwrap_or(());
        }
    }

    /// 清除选择
    fn clear_selection(&mut self) {
        if self.selection.is_some() {
            self.selection = None;
            self.refresh_screen().unwrap_or(());
        }
    }

    /// 复制选中的文本到系统剪贴板
    fn copy_selection(&mut self) {
        if let Some(selection) = self.selection {
            if selection.is_empty() {
                return;
            }

            let (start, end) = selection.normalized();
            let mut content = String::new();

            // 获取选中的文本
            let rows = self.rows.read().unwrap();
            if start.y == end.y {
                // 单行选择
                if let Some(row) = rows.get(start.y) {
                    let chars = row.string.chars().collect::<Vec<_>>();
                    let end_x = end.x.min(chars.len());
                    let selected: String = chars[start.x..end_x].iter().collect();
                    content.push_str(&selected);
                }
            } else {
                // 多行选择
                // 第一行
                if let Some(row) = rows.get(start.y) {
                    let chars = row.string.chars().collect::<Vec<_>>();
                    let selected: String = chars[start.x..].iter().collect();
                    content.push_str(&selected);
                    content.push('\n');
                }

                // 中间的行
                for y in (start.y + 1)..end.y {
                    if let Some(row) = rows.get(y) {
                        content.push_str(&row.string);
                        content.push('\n');
                    }
                }

                // 最后一行
                if let Some(row) = rows.get(end.y) {
                    let chars = row.string.chars().collect::<Vec<_>>();
                    let end_x = end.x.min(chars.len());
                    let selected: String = chars[..end_x].iter().collect();
                    content.push_str(&selected);
                }
            }

            // 保存到系统剪贴板
            if let Some(ctx) = self.sys_clipboard.as_mut() {
                if let Err(e) = ctx.set_contents(content.clone()) {
                    self.status_message = StatusMessage::from(
                        format!("无法复制到系统剪贴板: {}", e)
                    );
                    return;
                }
                self.status_message = StatusMessage::from(
                    format!("{} 个字符已复制到剪贴板", content.len())
                );
            } else {
                self.status_message = StatusMessage::from(
                    "系统剪贴板不可用".to_string()
                );
            }
        }
    }

    /// 删除选中的文本
    fn delete_selection(&mut self) {
        if let Some(selection) = self.selection {
            if selection.is_empty() {
                return;
            }

            let (start, end) = selection.normalized();
            // 先清除选择，避免后续的借用冲突
            self.clear_selection();
            
            let mut rows = self.rows.write().unwrap();

            // 如果选择在同一行内
            if start.y == end.y {
                let row = &mut rows[start.y];
                let mut result = String::new();
                let mut length = 0;
                for (index, grapheme) in row.string[..].graphemes(true).enumerate() {
                    if index < start.x || index >= end.x {
                        length += 1;
                        result.push_str(grapheme);
                    }
                }
                row.string = result;
                row.len = length;
                row.update_syntax();
            } else {
                // 处理多行选择
                // 保留第一行开始部分
                let mut first_line = String::new();
                let first_row = &rows[start.y];
                for (index, grapheme) in first_row.string[..].graphemes(true).enumerate() {
                    if index < start.x {
                        first_line.push_str(grapheme);
                    }
                }

                // 保留最后一行结束部分
                let mut last_line = String::new();
                let last_row = &rows[end.y];
                for (index, grapheme) in last_row.string[..].graphemes(true).enumerate() {
                    if index >= end.x {
                        last_line.push_str(grapheme);
                    }
                }

                // 合并第一行和最后一行
                first_line.push_str(&last_line);
                
                // 删除中间的行
                rows.drain(start.y + 1..=end.y);
                
                // 更新第一行
                rows[start.y] = Row::new(first_line);
            }

            // 更新光标位置到选择的开始位置
            self.cursor_position = start;
            self.dirty = true;
        }
    }

    /// 从系统剪贴板粘贴文本
    fn paste(&mut self) {
        // 从系统剪贴板获取内容
        let content = if let Some(ctx) = self.sys_clipboard.as_mut() {
            match ctx.get_contents() {
                Ok(text) => text,
                Err(e) => {
                    self.status_message = StatusMessage::from(
                        format!("无法从系统剪贴板获取内容: {}", e)
                    );
                    return;
                }
            }
        } else {
            self.status_message = StatusMessage::from(
                "系统剪贴板不可用".to_string()
            );
            return;
        };

        // 如果有选中的文本，先删除它
        if self.selection.is_some() {
            self.delete_selection();
        }

        // 按行分割文本
        let lines: Vec<&str> = content.split('\n').collect();
        
        if lines.is_empty() {
            return;
        }

        // 插入第一行
        for c in lines[0].chars() {
            self.insert_char(c);
        }

        // 插入剩余的行
        for line in lines.iter().skip(1) {
            self.insert_newline();
            for c in line.chars() {
                self.insert_char(c);
            }
        }

        self.status_message = StatusMessage::from(
            format!("已粘贴 {} 个字符", content.len())
        );
    }

    /// 启动搜索操作
    fn search(&mut self) -> io::Result<()> {
        let saved_cursor_position = self.cursor_position;
        let saved_offset = self.offset;

        self.is_searching = true;
        if let Some(_) = self.prompt("Search: ", Some(&Self::find_callback))? {
            // 如果用户按了Enter，保持搜索模式
            // 刷新屏幕以显示搜索高亮
            self.refresh_screen()?;
            // 状态消息已经在 find_callback 中设置
        } else {
            // 如果用户按了ESC，恢复原来的位置
            self.cursor_position = saved_cursor_position;
            self.offset = saved_offset;
            self.is_searching = false;
            self.current_search = None;
            self.search_state.last_match = None;
            self.status_message = StatusMessage::from(String::new());
            // 刷新屏幕以清除搜索高亮
            self.refresh_screen()?;
        }
        Ok(())
    }

    /// 移动光标
    fn move_cursor(&mut self, key: KeyCode) {
        let Position { mut x, mut y } = self.cursor_position;
        let rows = self.rows.read().unwrap();
        let height = rows.len();

        match key {
            KeyCode::Up => {
                if y > 0 {
                    y -= 1;
                    // 调整 x 坐标以适应新行的字符宽度
                    if let Some(row) = rows.get(y) {
                        let mut current_width = 0;
                        let mut new_x = 0;
                        let target_width = if let Some(current_row) = rows.get(self.cursor_position.y) {
                            current_row.string[..]
                                .graphemes(true)
                                .take(x)
                                .map(|g| UnicodeWidthStr::width(g))
                                .sum::<usize>()
                        } else {
                            0
                        };

                        for (i, grapheme) in row.string[..].graphemes(true).enumerate() {
                            let width = UnicodeWidthStr::width(grapheme);
                            if current_width > target_width {
                                break;
                            }
                            current_width += width;
                            new_x = i;
                        }
                        x = new_x;
                    }
                }
            }
            KeyCode::Down => {
                if y < height {
                    y += 1;
                    // 调整 x 坐标以适应新行的字符宽度
                    if let Some(row) = rows.get(y) {
                        let mut current_width = 0;
                        let mut new_x = 0;
                        let target_width = if let Some(current_row) = rows.get(self.cursor_position.y) {
                            current_row.string[..]
                                .graphemes(true)
                                .take(x)
                                .map(|g| UnicodeWidthStr::width(g))
                                .sum::<usize>()
                        } else {
                            0
                        };

                        for (i, grapheme) in row.string[..].graphemes(true).enumerate() {
                            let width = UnicodeWidthStr::width(grapheme);
                            if current_width > target_width {
                                break;
                            }
                            current_width += width;
                            new_x = i;
                        }
                        x = new_x;
                    }
                }
            }
            KeyCode::Left => {
                if x > 0 {
                    x -= 1;
                } else if y > 0 {
                    y -= 1;
                    if let Some(row) = rows.get(y) {
                        x = row.len;
                    } else {
                        x = 0;
                    }
                }
            }
            KeyCode::Right => {
                if let Some(row) = rows.get(y) {
                    if x < row.len {
                        x += 1;
                    } else if y < height {
                        y += 1;
                        x = 0;
                    }
                }
            }
            KeyCode::PageUp => {
                y = if y > self.screen_rows {
                    y - self.screen_rows
                } else {
                    0
                }
            }
            KeyCode::PageDown => {
                y = if y.saturating_add(self.screen_rows) < height {
                    y + self.screen_rows
                } else {
                    height
                }
            }
            KeyCode::Home => x = 0,
            KeyCode::End => {
                if let Some(row) = rows.get(y) {
                    x = row.len;
                }
            }
            _ => (),
        }

        // 确保 x 不超过当前行的长度
        let width = if let Some(row) = rows.get(y) {
            row.len
        } else {
            0
        };
        if x > width {
            x = width;
        }

        self.cursor_position = Position { x, y }
    }

    /// 处理按键事件
    fn process_keypress(&mut self) -> io::Result<()> {
        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(key_event) => {
                    if key_event.kind == KeyEventKind::Press {
                        match (key_event.code, key_event.modifiers) {
                            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                                if self.dirty && self.quit_times > 0 {
                                    self.status_message = StatusMessage::error(format!(
                                        "WARNING!!! File has unsaved changes. Press Ctrl-Q {} more times to quit.",
                                        self.quit_times
                                    ));
                                    self.quit_times -= 1;
                                    return Ok(());
                                }
                                self.should_quit = true;
                            }
                            (KeyCode::Char('s'), KeyModifiers::CONTROL) => self.save()?,
                            (KeyCode::Char('f'), KeyModifiers::CONTROL) => self.search()?,
                            (KeyCode::Char('h'), KeyModifiers::CONTROL) => self.replace()?,
                            // 复制选中文本
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                if self.selection.is_some() {
                                    self.copy_selection();
                                }
                            }
                            // 剪切选中文本
                            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                                if self.selection.is_some() {
                                    self.copy_selection();
                                    self.delete_selection();
                                }
                            }
                            // 粘贴文本
                            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                                self.paste();
                            }
                            // 删除选中文本
                            (KeyCode::Delete, _) | (KeyCode::Backspace, _) => {
                                if self.selection.is_some() {
                                    self.delete_selection();
                                } else {
                                    self.delete_char();
                                }
                            }
                            (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                                if self.selection.is_none() {
                                    self.start_selection();
                                }
                                self.insert_char(c);
                                self.update_selection();
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE) => {
                                self.clear_selection();
                                self.insert_char(c);
                            }
                            (KeyCode::Enter, _) => {
                                if self.selection.is_some() {
                                    self.delete_selection();
                                }
                                self.insert_newline();
                            }
                            (KeyCode::Up, mods) | (KeyCode::Down, mods) |
                            (KeyCode::Left, mods) | (KeyCode::Right, mods) => {
                                if mods.contains(KeyModifiers::SHIFT) {
                                    if self.selection.is_none() {
                                        self.start_selection();
                                    }
                                    self.move_cursor(key_event.code);
                                    self.update_selection();
                                } else {
                                    self.clear_selection();
                                    self.move_cursor(key_event.code);
                                }
                            }
                            (KeyCode::PageUp, _) => {
                                self.clear_selection();
                                self.move_cursor(KeyCode::PageUp);
                            }
                            (KeyCode::PageDown, _) => {
                                self.clear_selection();
                                self.move_cursor(KeyCode::PageDown);
                            }
                            (KeyCode::Home, _) => {
                                self.clear_selection();
                                self.move_cursor(KeyCode::Home);
                            }
                            (KeyCode::End, _) => {
                                self.clear_selection();
                                self.move_cursor(KeyCode::End);
                            }
                            _ => (),
                        }
                        self.quit_times = QUIT_TIMES;
                    }
                }
                Event::Mouse(event) => {
                    match event.kind {
                        event::MouseEventKind::Down(event::MouseButton::Left) => {
                            let Position { x: offset_x, y: offset_y } = self.offset;
                            let x = event.column as usize + offset_x;
                            let y = event.row as usize + offset_y;
                            
                            // 获取行数并立即释放锁
                            let row_len = {
                                let rows = self.rows.read().unwrap();
                                if y >= rows.len() {
                                    return Ok(());
                                }
                                let row = &rows[y];
                                row.len
                            };
                            
                            let x = x.min(row_len);
                            self.cursor_position = Position { x, y };
                            self.clear_selection();
                        }
                        event::MouseEventKind::Drag(event::MouseButton::Left) => {
                            let Position { x: offset_x, y: offset_y } = self.offset;
                            let x = event.column as usize + offset_x;
                            let y = event.row as usize + offset_y;
                            
                            // 获取行数并立即释放锁
                            let row_len = {
                                let rows = self.rows.read().unwrap();
                                if y >= rows.len() {
                                    return Ok(());
                                }
                                let row = &rows[y];
                                row.len
                            };
                            
                            let x = x.min(row_len);
                            if self.selection.is_none() {
                                self.start_selection();
                            }
                            self.cursor_position = Position { x, y };
                            self.update_selection();
                        }
                        _ => (),
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }

    /// 处理屏幕滚动
    fn scroll(&mut self) {
        let Position { x, y } = self.cursor_position;
        let width = self.screen_cols;
        let height = self.screen_rows;

        let offset = &mut self.offset;
        if y < offset.y {
            offset.y = y;
        } else if y >= offset.y.saturating_add(height) {
            offset.y = y.saturating_sub(height).saturating_add(1);
        }

        // 计算当前行的显示宽度
        let mut current_width = 0;
        let mut target_x = 0;
        if let Some(row) = self.rows.read().unwrap().get(y) {
            for (i, grapheme) in row.string[..].graphemes(true).enumerate() {
                let char_width = UnicodeWidthStr::width(grapheme);
                if i == x {
                    target_x = current_width;
                    break;
                }
                current_width += char_width;
            }
        }

        if target_x < offset.x {
            offset.x = target_x;
        } else if target_x >= offset.x.saturating_add(width) {
            offset.x = target_x.saturating_sub(width).saturating_add(1);
        }
    }

    /// 刷新屏幕显示
    fn refresh_screen(&mut self) -> io::Result<()> {
        self.scroll();

        // 在搜索模式下，确保状态消息不会被清除
        if !self.is_searching {
            // 只有在非搜索模式下才检查消息超时
            if self.status_message.message_type == StatusMessageType::Normal && 
               !self.status_message.persistent && 
               self.status_message.time.elapsed() > Duration::from_secs(5) {
                self.status_message = StatusMessage::from(String::new());
            }
        }
        
        queue!(
            stdout(),
            terminal::Clear(ClearType::All),
            cursor::Hide,
            cursor::MoveTo(0, 0)
        )?;
        
        self.draw_rows()?;
        self.draw_status_bar()?;
        self.draw_message_bar()?;
        
        let Position { x, y } = self.cursor_position;
        let Position { x: offset_x, y: offset_y } = self.offset;
        
        let display_x = if let Some(row) = self.rows.read().unwrap().get(y) {
            row.string[..]
                .graphemes(true)
                .take(x)
                .map(|g| UnicodeWidthStr::width(g))
                .sum::<usize>()
        } else {
            x
        };
        
        let cursor_x = display_x.saturating_sub(offset_x);
        let cursor_y = y.saturating_sub(offset_y);
        
        queue!(
            stdout(),
            cursor::MoveTo(cursor_x as u16, cursor_y as u16),
            cursor::Show
        )?;
        
        stdout().flush()
    }

    /// 绘制状态栏
    fn draw_status_bar(&mut self) -> io::Result<()> {
        let width = self.screen_cols;
        
        let modified_indicator = if self.dirty { "(modified)" } else { "" };
        let mut file_name = "[No Name]".to_string();
        if let Some(name) = &self.filename {
            file_name = name.clone();
            if file_name.len() > 20 {
                file_name.truncate(20);
            }
        }
        
        let mut status = format!(
            "{} - {} lines {}",
            file_name,
            self.rows.read().unwrap().len(),
            modified_indicator
        );

        // 添加搜索模式指示
        if self.is_searching {
            status.push_str(" | SEARCH MODE");
        }
        
        let line_indicator = format!(
            "{}:{}/{}",
            self.cursor_position.y.saturating_add(1),
            self.cursor_position.x.saturating_add(1),
            self.rows.read().unwrap().len()
        );
        
        let len = status.len() + line_indicator.len();
        status.push_str(&" ".repeat(width.saturating_sub(len)));
        status = format!("{}{}", status, line_indicator);
        status.truncate(width);
        
        queue!(
            stdout(),
            style::SetAttribute(style::Attribute::Reverse),
            cursor::MoveTo(0, self.screen_rows as u16),
            terminal::Clear(ClearType::CurrentLine),
            Print(&status),
            style::SetAttribute(style::Attribute::Reset)
        )?;
        
        Ok(())
    }

    /// 绘制消息栏
    fn draw_message_bar(&mut self) -> io::Result<()> {
        queue!(
            stdout(),
            cursor::MoveTo(0, (self.screen_rows + 1) as u16),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        
        let mut text = self.status_message.text.clone();
        text.truncate(self.screen_cols);

        // 根据消息类型设置不同的显示样式
        match self.status_message.message_type {
            StatusMessageType::Error => {
                queue!(
                    stdout(),
                    style::SetForegroundColor(style::Color::Red),
                    style::SetAttribute(style::Attribute::Bold),
                    Print(&text),
                    style::ResetColor,
                    style::SetAttribute(style::Attribute::Reset)
                )?;
            }
            StatusMessageType::Search => {
                queue!(
                    stdout(),
                    style::SetForegroundColor(style::Color::Yellow),
                    style::SetAttribute(style::Attribute::Bold),
                    Print(&text),
                    style::ResetColor,
                    style::SetAttribute(style::Attribute::Reset)
                )?;
            }
            StatusMessageType::Normal => {
                queue!(stdout(), Print(&text))?;
            }
        }
        
        Ok(())
    }

    /// 渲染单行文本
    fn render_row(&self, row: &Row) -> String {
        let mut result = String::new();
        let mut current_display_width = 0;
        let mut skip_chars = self.offset.x;

        let mut current_highlighting = HighlightType::Normal;
        let mut is_in_search_highlight = false;
        let mut is_in_selection = false;

        // 获取选择范围
        let selection_range = self.selection.map(|s| s.normalized());

        // 首先收集所有搜索匹配的位置
        let mut search_highlights = Vec::new();
        if let Some(ref search_text) = self.current_search {
            if !search_text.is_empty() {
                let mut index = 0;
                while let Some(found_index) = row.search(search_text, index) {
                    search_highlights.push((found_index, found_index + search_text.len()));
                    index = found_index + 1;
                }
            }
        }

        // 遍历并渲染每个字符
        for (index, grapheme) in row.string[..].graphemes(true).enumerate() {
            let char_width = UnicodeWidthStr::width(grapheme);
            
            // 跳过偏移之前的字符
            if skip_chars > 0 {
                skip_chars -= 1;
                current_display_width += char_width;
                continue;
            }

            // 检查是否超出屏幕宽度
            if current_display_width + char_width > self.screen_cols {
                break;
            }

            // 检查是否在选择范围内
            if let Some((sel_start, sel_end)) = selection_range {
                let current_pos = Position { x: index, y: self.cursor_position.y };
                let in_selection = if sel_start.y == sel_end.y {
                    // 单行选择
                    current_pos.y == sel_start.y && index >= sel_start.x && index < sel_end.x
                } else {
                    // 多行选择
                    (current_pos.y == sel_start.y && index >= sel_start.x) ||  // 第一行
                    (current_pos.y > sel_start.y && current_pos.y < sel_end.y) ||  // 中间行
                    (current_pos.y == sel_end.y && index < sel_end.x)  // 最后一行
                };

                if in_selection != is_in_selection {
                    is_in_selection = in_selection;
                    if in_selection {
                        result.push_str("\x1b[7m"); // 反转显示（背景色和前景色交换）
                    } else {
                        result.push_str("\x1b[27m"); // 取消反转
                    }
                }
            }

            // 检查是否在搜索高亮范围内
            let in_search = search_highlights.iter()
                .any(|&(start, end)| index >= start && index < end);

            // 获取语法高亮类型
            if let Some(&highlighting_type) = row.highlighting.get(index) {
                if highlighting_type != current_highlighting {
                    current_highlighting = highlighting_type;
                    if !in_search && !is_in_selection {
                        let color = current_highlighting.to_color();
                        result.push_str(&format!("\x1b[38;5;{}m", color));
                    }
                }
            }

            // 处理搜索高亮
            if in_search != is_in_search_highlight {
                is_in_search_highlight = in_search;
                if in_search {
                    result.push_str("\x1b[43m"); // 黄色背景
                } else {
                    result.push_str("\x1b[49m"); // 恢复默认背景
                    // 恢复当前语法高亮的前景色
                    if !is_in_selection {
                        let color = current_highlighting.to_color();
                        result.push_str(&format!("\x1b[38;5;{}m", color));
                    }
                }
            }

            // 渲染字符
            if grapheme == "\t" {
                result.push_str("    ");
                current_display_width += 4;
            } else {
                result.push_str(grapheme);
                current_display_width += char_width;
            }
        }

        result.push_str("\x1b[0m");
        result
    }

    /// 绘制所有行
    fn draw_rows(&mut self) -> io::Result<()> {
        let height = self.screen_rows;
        let rows = self.rows.read().unwrap();
        for terminal_row in 0..height {
            let file_row = terminal_row + self.offset.y;
            if file_row >= rows.len() {
                if rows.is_empty() && terminal_row == height / 3 {
                    let welcome = format!("Hecto editor -- version {}", VERSION);
                    let padding = (self.screen_cols - welcome.len()) / 2;
                    if padding > 0 {
                        queue!(stdout(), Print("~"))?;
                        for _ in 0..padding - 1 {
                            queue!(stdout(), Print(" "))?;
                        }
                        queue!(stdout(), Print(&welcome))?;
                    } else {
                        queue!(stdout(), Print("~"))?;
                    }
                } else {
                    queue!(stdout(), Print("~"))?;
                }
            } else {
                let row = &rows[file_row];
                // 临时保存当前光标位置的 y 坐标
                let saved_y = self.cursor_position.y;
                // 设置当前渲染行的 y 坐标
                self.cursor_position.y = file_row;
                let rendered_row = self.render_row(row);
                // 恢复光标位置的 y 坐标
                self.cursor_position.y = saved_y;
                queue!(stdout(), Print(&rendered_row))?;
            }
            queue!(
                stdout(),
                terminal::Clear(ClearType::UntilNewLine)
            )?;
            if terminal_row < height - 1 {
                queue!(stdout(), Print("\r\n"))?;
            }
        }
        Ok(())
    }

    /// 运行编辑器
    fn run(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        
        // 设置终端
        crossterm::execute!(
            stdout(),
            terminal::EnterAlternateScreen,
            event::EnableMouseCapture,
            terminal::SetTitle("Hecto Editor"),
            terminal::DisableLineWrap
        )?;

        // 禁用快速编辑模式（Windows特定）
        #[cfg(windows)]
        {
            use crossterm::event::EnableFocusChange;
            crossterm::execute!(stdout(), EnableFocusChange)?;
        }

        let result = self.run_loop();

        // 恢复终端设置
        crossterm::execute!(
            stdout(),
            event::DisableMouseCapture,
            terminal::EnableLineWrap,
            terminal::LeaveAlternateScreen
        )?;
        terminal::disable_raw_mode()?;
        
        result
    }

    /// 主循环
    fn run_loop(&mut self) -> io::Result<()> {
        loop {
            if let Err(error) = self.refresh_screen() {
                die(&error);
            }
            if self.should_quit {
                break;
            }
            if let Err(error) = self.process_keypress() {
                die(&error);
            }
        }
        Ok(())
    }
}

/// 处理致命错误
fn die(e: &io::Error) {
    terminal::disable_raw_mode().unwrap();
    eprintln!("Error: {}", e);
    std::process::exit(1);
}

/// 程序入口点
fn main() -> io::Result<()> {
    let mut editor = Editor::new();
    if let Some(filename) = std::env::args().nth(1) {
        editor.open(&filename)?;
    }
    editor.run()
}

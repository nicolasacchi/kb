//! A tiny screen model for unit tests: just enough VT to tell what the
//! mirror's bytes leave on a terminal (printable text, CR/LF with scrolling,
//! autowrap, CUP, ED 2/3, EL, DECSC/DECRC, and SGR/mode/region sequences
//! that change no cell). It is a test oracle, not an emulator.

#[derive(Debug, Clone)]
pub struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<char>>,
    cx: usize,
    cy: usize,
    saved: (usize, usize),
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        Screen {
            cols,
            rows,
            cells: vec![vec![' '; cols]; rows],
            cx: 0,
            cy: 0,
            saved: (0, 0),
        }
    }

    /// Row `i` as text, trailing blanks trimmed.
    pub fn row(&self, i: usize) -> String {
        self.cells[i]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cx, self.cy)
    }

    fn linefeed(&mut self) {
        if self.cy + 1 == self.rows {
            self.cells.remove(0);
            self.cells.push(vec![' '; self.cols]);
        } else {
            self.cy += 1;
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => match it.next() {
                    Some('7') => self.saved = (self.cx, self.cy),
                    Some('8') => (self.cx, self.cy) = self.saved,
                    Some('[') => {
                        let mut params = String::new();
                        let mut fin = ' ';
                        for n in it.by_ref() {
                            if ('\x40'..='\x7e').contains(&n) {
                                fin = n;
                                break;
                            }
                            params.push(n);
                        }
                        self.csi(&params, fin);
                    }
                    _ => {}
                },
                '\r' => self.cx = 0,
                '\n' => self.linefeed(),
                c if c.is_control() => {}
                c => {
                    if self.cx == self.cols {
                        self.cx = 0;
                        self.linefeed();
                    }
                    self.cells[self.cy][self.cx] = c;
                    self.cx += 1;
                }
            }
        }
    }

    fn csi(&mut self, params: &str, fin: char) {
        let nums: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let n0 = nums.first().copied().unwrap_or(0);
        match fin {
            'H' | 'f' => {
                let row = nums.first().copied().unwrap_or(1).max(1);
                let col = nums.get(1).copied().unwrap_or(1).max(1);
                self.cy = (row - 1).min(self.rows - 1);
                self.cx = (col - 1).min(self.cols - 1);
            }
            'J' if n0 == 2 => {
                self.cells = vec![vec![' '; self.cols]; self.rows];
            }
            'K' => {
                for x in self.cx..self.cols {
                    self.cells[self.cy][x] = ' ';
                }
            }
            // DECSTBM homes the cursor.
            'r' if !params.starts_with('?') => {
                self.cx = 0;
                self.cy = 0;
            }
            _ => {}
        }
    }
}

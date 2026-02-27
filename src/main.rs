use core::{
	error::Error,
	num::{NonZeroU32, NonZeroUsize}
};
use std::{
	borrow::Cow,
	ffi::OsString,
	io::{BufReader, Read as _, Stdout, Write as _, stdout},
	mem,
	path::PathBuf,
	sync::{Arc, Mutex},
	time::Duration
};

use crossterm::{
	event::EventStream,
	execute,
	terminal::{
		EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
		enable_raw_mode, window_size
	}
};
use debounce::EventDebouncer;
use flexi_logger::FileSpec;
use flume::{Sender, r#async::RecvStream};
use futures_util::{FutureExt as _, stream::StreamExt as _};
use kittage::{
	action::Action,
	delete::{ClearOrDelete, DeleteConfig, WhichToDelete},
	error::{TerminalError, TransmitError}
};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_image::{
	FontSize,
	picker::{Picker, ProtocolType}
};
use pdftui::{
	PrerenderLimit,
	converter::{ConvertedPage, ConverterMsg, run_conversion_loop},
	kitty::{KittyDisplay, display_kitty_images, do_shms_work, run_action},
	renderer::{self, MUPDF_BLACK, MUPDF_WHITE, RenderError, RenderInfo, RenderNotif},
	synctex,
	tui::{BottomMessage, InputAction, MessageSetting, Tui}
};

// Dummy struct for easy errors in main
struct WrappedErr(Cow<'static, str>);

impl std::fmt::Display for WrappedErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

impl std::fmt::Debug for WrappedErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(self, f)
	}
}

impl std::error::Error for WrappedErr {}

fn reset_term() {
	_ = disable_raw_mode();
	_ = execute!(
		std::io::stdout(),
		LeaveAlternateScreen,
		crossterm::cursor::Show,
		crossterm::event::DisableMouseCapture
	);
}

#[tokio::main]
async fn main() -> Result<(), WrappedErr> {
	let result = inner_main().await;
	reset_term();
	result
}

async fn inner_main() -> Result<(), WrappedErr> {
	let hook = std::panic::take_hook();
	std::panic::set_hook(Box::new(move |info| {
		reset_term();
		hook(info);
	}));

	#[cfg(feature = "tracing")]
	console_subscriber::init();

	const DEFAULT_DEBOUNCE_DELAY: Duration = Duration::from_millis(50);

	let flags = xflags::parse_or_exit! {
		/// Display the pdf with the pages starting at the right hand size and moving left and
		/// adjust input keys to match
		optional -r,--r-to-l
		/// The maximum number of pages to display together, horizontally, at a time
		optional -m,--max-wide max_wide: NonZeroUsize
		/// Fullscreen the pdf (hide document name, page count, etc)
		optional -f,--fullscreen
		/// The time to wait for the file to stop changing before reloading, in milliseconds.
		/// Defaults to 50ms.
		optional --reload-delay reload_delay: u64
		/// The number of pages to prerender surrounding the currently-shown page; 0 means no
		/// limit. By default, there is no limit.
		optional -p,--prerender prerender: usize
		/// Custom white color, specified in css format (e.g. "FFFFFF" or "rgb(255, 255, 255)")
		optional -w,--white-color white: String
		/// Custom black color, specified in css format (e.g "000000" or "rgb(0, 0, 0)")
		optional -b,--black-color black: String
		/// Print the version and exit
		optional --version
		/// SyncTeX forward search: jump to the PDF position corresponding to line:col:file
		/// (e.g. "42:0:main.tex")
		optional --synctex-forward synctex_forward: String
		/// Command to run on inverse search (Ctrl+click). Use {file}, {line}, {col} as placeholders.
		/// (e.g. "nvr --remote-silent +{line} {file}")
		optional --inverse-cmd inverse_cmd: String
		/// Print the IPC socket path for the given PDF and exit
		optional --socket-path socket_path: PathBuf
		/// Forward search to a running pdftui instance: line:col:file (e.g. "42:0:main.tex")
		optional --forward forward: String
		/// PDF file to read
		optional file: PathBuf
	};

	if flags.version {
		print!("{}", env!("CARGO_PKG_VERSION"));
		std::process::exit(0);
	}

	if let Some(ref sp) = flags.socket_path {
		let p = sp.canonicalize().unwrap_or_else(|_| sp.clone());
		print!("{}", pdftui::ipc::socket_path(&p).display());
		std::process::exit(0);
	}

	// Client mode: send forward search to a running instance and exit
	if let Some(ref fwd) = flags.forward {
		let pdf = flags.file.as_ref().ok_or_else(|| {
			WrappedErr("--forward requires a PDF file argument".into())
		})?;
		let pdf = pdf.canonicalize().unwrap_or_else(|_| pdf.clone());
		let (line, col, file) = parse_synctex_arg(fwd).map_err(|e| {
			WrappedErr(format!("invalid --forward argument: {e}").into())
		})?;
		let response = pdftui::ipc::send_forward(&pdf, line as u32, col as u32, &file)
			.await
			.map_err(|e| WrappedErr(e.into()))?;
		println!("{response}");
		std::process::exit(0);
	}

	let Some(file) = flags.file else {
		return Err(WrappedErr(
			"Please specify the file to open, e.g. `pdftui ./my_example_pdf.pdf`".into()
		));
	};

	let path = file
		.canonicalize()
		.map_err(|e| WrappedErr(format!("Cannot canonicalize provided file: {e}").into()))?;

	let black = flags
		.black_color
		.as_deref()
		.map(|color| {
			parse_color_to_i32(color).map_err(|e| {
				WrappedErr(
					format!(
						"Couldn't parse black color {color:?}: {e} - is it formatted like a CSS color?"
					)
					.into()
				)
			})
		})
		.transpose()?
		.unwrap_or(MUPDF_BLACK);

	let white = flags
		.white_color
		.as_deref()
		.map(|color| {
			parse_color_to_i32(color).map_err(|e| {
				WrappedErr(
					format!(
						"Couldn't parse white color {color:?}: {e} - is it formatted like a CSS color?"
					)
					.into()
				)
			})
		})
		.transpose()?
		.unwrap_or(MUPDF_WHITE);

	// need to keep it around throughout the lifetime of the program, but don't rly need to use it.
	// Just need to make sure it doesn't get dropped yet.
	let maybe_logger = if std::env::var("RUST_LOG").is_ok() {
		Some(
			flexi_logger::Logger::try_with_env()
				.map_err(|e| WrappedErr(format!("Couldn't create initial logger: {e}").into()))?
				.log_to_file(FileSpec::try_from("./debug.log").map_err(|e| {
					WrappedErr(format!("Couldn't create FileSpec for logger: {e}").into())
				})?)
				.start()
				.map_err(|e| WrappedErr(format!("Can't start logger: {e}").into()))?
		)
	} else {
		None
	};

	let (watch_to_render_tx, render_rx) = flume::unbounded();
	let to_renderer = watch_to_render_tx.clone();

	let (render_tx, tui_rx) = flume::unbounded();
	let watch_to_tui_tx = render_tx.clone();

	let mut watcher = notify::recommended_watcher(on_notify_ev(
		watch_to_tui_tx,
		watch_to_render_tx,
		path.file_name()
			.ok_or_else(|| WrappedErr("Path does not have a last component??".into()))?
			.to_owned(),
		flags
			.reload_delay
			.map_or(DEFAULT_DEBOUNCE_DELAY, Duration::from_millis)
	))
	.map_err(|e| WrappedErr(format!("Couldn't start watching the provided file: {e}").into()))?;

	// So we have to watch the parent directory of the file that we are interested in because the
	// `notify` library works on inodes, and if the file is deleted, that inode is gone as well,
	// and then the notify library just gives up on trying to watch for the file reappearing. Imo
	// they should start watching the parent directory if the file is deleted, and then wait for it
	// to reappear and then begin watching it again, but whatever. It seems they've made their
	// opinion on this clear
	// (https://github.com/notify-rs/notify/issues/113#issuecomment-281836995) so whatever, guess
	// we have to do this annoying workaround.
	watcher
		.watch(
			path.parent().expect("The root directory is not a PDF"),
			RecursiveMode::NonRecursive
		)
		.map_err(|e| WrappedErr(format!("Can't watch the provided file: {e}").into()))?;

	let mut window_size = window_size().map_err(|e| {
		WrappedErr(format!("Can't get your current terminal window size: {e}").into())
	})?;

	if window_size.width == 0 || window_size.height == 0 {
		let (w, h) = get_font_size_through_stdio()?;

		window_size.width = w;
		window_size.height = h;
	}

	let cell_height_px = window_size.height / window_size.rows;
	let cell_width_px = window_size.width / window_size.columns;

	execute!(
		std::io::stdout(),
		EnterAlternateScreen,
		crossterm::cursor::Hide,
		crossterm::event::EnableMouseCapture
	)
	.map_err(|e| {
		WrappedErr(
			format!(
				"Couldn't enter the alternate screen and hide the cursor for proper presentation: {e}"
			)
			.into()
		)
	})?;

	// We need to create `picker` on this thread because if we create it on the `renderer` thread,
	// it messes up something with user input. Input never makes it to the crossterm thing
	let picker = Picker::from_query_stdio()
		.or_else(|e| match e {
			ratatui_image::errors::Errors::NoFontSize if
				window_size.width != 0
				&& window_size.height != 0
				&& window_size.columns != 0
				&& window_size.rows != 0 =>
			{
				// the 'equivalent' that is suggested instead is not the same. We need to keep
				// calling this.
				#[expect(deprecated)]
				Ok(Picker::from_fontsize((cell_width_px, cell_height_px)))
			},
			ratatui_image::errors::Errors::NoFontSize => Err(WrappedErr(
				"Unable to detect your terminal's font size; this is an issue with your terminal emulator.\nPlease use a different terminal emulator or report this bug to pdftui.".into()
			)),
			e => Err(WrappedErr(format!("Couldn't get the necessary information to set up images: {e}").into()))
		})?;

	// then we want to spawn off the rendering task
	// We need to use the thread::spawn API so that this exists in a thread not owned by tokio,
	// since the methods we call in `start_rendering` will panic if called in an async context
	let prerender = flags
		.prerender
		.and_then(NonZeroUsize::new)
		.map_or(PrerenderLimit::All, PrerenderLimit::Limited);

	let file_path = path.clone();
	std::thread::spawn(move || {
		renderer::start_rendering(
			&file_path,
			render_tx,
			render_rx,
			cell_height_px,
			cell_width_px,
			prerender,
			black,
			white
		)
	});

	let font_size = picker.font_size();

	let mut ev_stream = crossterm::event::EventStream::new();

	let (to_converter, from_main) = flume::unbounded();
	let (to_main, from_converter) = flume::unbounded();

	let is_kitty = picker.protocol_type() == ProtocolType::Kitty;

	let shms_work = is_kitty && do_shms_work(&mut ev_stream).await;

	tokio::spawn(run_conversion_loop(
		to_main, from_main, picker, 20, shms_work
	));

	let file_name = path.file_name().map_or_else(
		|| "Unknown file".into(),
		|n| n.to_string_lossy().to_string()
	);
	let mut tui = Tui::new(file_name, flags.max_wide, flags.r_to_l, is_kitty, cell_width_px, cell_height_px);

	let backend = CrosstermBackend::new(std::io::stdout());
	let mut term = Terminal::new(backend).map_err(|e| {
		WrappedErr(format!("Couldn't set up crossterm's terminal backend: {e}").into())
	})?;
	term.skip_diff(true);

	enable_raw_mode().map_err(|e| {
		WrappedErr(
			format!("Can't enable raw mode, which is necessary to receive input: {e}").into()
		)
	})?;

	if is_kitty {
		run_action(
			Action::Delete(DeleteConfig {
				effect: ClearOrDelete::Delete,
				which: WhichToDelete::IdRange(NonZeroU32::new(1).unwrap()..=NonZeroU32::MAX)
			}),
			&mut ev_stream
		)
		.await
		.map_err(|e| {
			WrappedErr(format!("Couldn't delete all previous images from memory: {e}").into())
		})?;
	}

	let fullscreen = flags.fullscreen;
	let main_area = Tui::main_layout(&term.get_frame(), fullscreen);
	to_renderer
		.send(RenderNotif::Area(main_area.page_area))
		.map_err(|e| {
			WrappedErr(
				format!("Couldn't inform the rendering thread of the available area: {e}").into()
			)
		})?;

	// Handle SyncTeX forward search if requested
	if let Some(ref synctex_arg) = flags.synctex_forward {
		match parse_synctex_arg(synctex_arg) {
			Ok((line, col, input_file)) => {
				match synctex::forward_search(line, col, &input_file, &path) {
					Ok(result) => {
						to_renderer
							.send(RenderNotif::SyncTexJump {
								page: result.page,
								h: result.h,
								v: result.v,
								width: result.width,
								height: result.height
							})
							.map_err(|e| {
								WrappedErr(
									format!("Couldn't send SyncTeX jump to renderer: {e}").into()
								)
							})?;
						to_converter
							.send(ConverterMsg::GoToPage(result.page))
							.map_err(|e| {
								WrappedErr(
									format!("Couldn't send page jump to converter: {e}").into()
								)
							})?;
						tui.page = result.page;
					}
					Err(e) => {
						tui.set_msg(MessageSetting::Some(BottomMessage::Error(
							format!("SyncTeX: {e}")
						)));
					}
				}
			}
			Err(e) => {
				return Err(WrappedErr(
					format!("Invalid --synctex-forward argument: {e}").into()
				));
			}
		}
	}

	let tui_rx = tui_rx.into_stream();
	let from_converter = from_converter.into_stream();

	// Start IPC listener if synctex file exists
	let (ipc_page_rx, ipc_socket_path) = if synctex::has_synctex_file(&path) {
		let (sock_path, rx) = pdftui::ipc::start_ipc_listener(
			&path,
			to_renderer.clone(),
			to_converter.clone()
		);
		eprintln!("IPC socket: {}", sock_path.display());
		(Some(rx.into_stream()), Some(sock_path))
	} else {
		(None, None)
	};

	let result = enter_redraw_loop(
		ev_stream,
		to_renderer,
		tui_rx,
		to_converter,
		from_converter,
		fullscreen,
		tui,
		&mut term,
		main_area,
		font_size,
		flags.inverse_cmd,
		path,
		ipc_page_rx
	)
	.await
	.map_err(|e| {
		WrappedErr(
			format!(
				"An unexpected error occurred while communicating between different parts of pdftui: {e}"
			)
			.into()
		)
	});

	// Cleanup IPC socket
	if let Some(sock) = ipc_socket_path {
		pdftui::ipc::cleanup_socket(&sock);
	}

	drop(maybe_logger);
	result
}

// oh shut up clippy who cares
#[expect(clippy::too_many_arguments)]
async fn enter_redraw_loop(
	mut ev_stream: EventStream,
	to_renderer: Sender<RenderNotif>,
	mut tui_rx: RecvStream<'_, Result<RenderInfo, RenderError>>,
	to_converter: Sender<ConverterMsg>,
	mut from_converter: RecvStream<'_, Result<ConvertedPage, RenderError>>,
	mut fullscreen: bool,
	mut tui: Tui,
	term: &mut Terminal<CrosstermBackend<Stdout>>,
	mut main_area: pdftui::tui::RenderLayout,
	font_size: FontSize,
	inverse_cmd: Option<String>,
	pdf_path: PathBuf,
	mut ipc_page_rx: Option<flume::r#async::RecvStream<'_, usize>>
) -> Result<(), Box<dyn Error>> {
	loop {
		let mut needs_redraw = true;
		let next_ev = ev_stream.next().fuse();
		tokio::select! {
			// First we check if we have any keystrokes
			Some(ev) = next_ev => {
				// If we can't get user input, just crash.
				let ev = ev.expect("Couldn't get any user input");

				match tui.handle_event(&ev) {
					None => needs_redraw = false,
					Some(action) => match action {
						InputAction::Redraw => (),
						InputAction::QuitApp => return Ok(()),
						InputAction::JumpingToPage(page) => {
							to_renderer.send(RenderNotif::JumpToPage(page))?;
							to_converter.send(ConverterMsg::GoToPage(page))?;
						},
						InputAction::Search(term) => to_renderer.send(RenderNotif::Search(term))?,
						InputAction::Invert => to_renderer.send(RenderNotif::Invert)?,
						InputAction::Rotate => to_renderer.send(RenderNotif::Rotate)?,
						InputAction::Fullscreen => fullscreen = !fullscreen,
						InputAction::SwitchRenderZoom(f_or_f) => {
							to_renderer.send(RenderNotif::SwitchFitOrFill(f_or_f)).unwrap();
						},
						InputAction::InverseSearch { page, pdf_x, pdf_y } => {
							// synctex edit uses 1-based pages
							match synctex::inverse_search(page + 1, pdf_x, pdf_y, &pdf_path) {
								Ok(result) => {
									if let Some(ref cmd) = inverse_cmd {
										let expanded = cmd
											.replace("{file}", &result.input)
											.replace("{line}", &result.line.to_string())
											.replace("{col}", &result.column.to_string());
										let _ = std::process::Command::new("sh")
											.args(["-c", &expanded])
											.spawn();
									}
									tui.set_msg(MessageSetting::Some(BottomMessage::Error(
										format!("SyncTeX → {}:{}", result.input, result.line)
									)));
								}
								Err(synctex::SyncTexError::NotFound) => {
									tui.set_msg(MessageSetting::Some(BottomMessage::Error(
										"synctex not found (install texlive)".into()
									)));
								}
								Err(e) => {
									tui.set_msg(MessageSetting::Some(BottomMessage::Error(
										format!("SyncTeX inverse: {e}")
									)));
								}
							}
						}
					}
				}
			},
			Some(renderer_msg) = tui_rx.next() => {
				match renderer_msg {
					Ok(render_info) => match render_info {
						RenderInfo::NumPages(num) => {
							tui.set_n_pages(num);
							to_converter.send(ConverterMsg::NumPages(num))?;
						},
						RenderInfo::Page(info) => {
							tui.got_num_results_on_page(info.page_num, info.result_rects.len());
							to_converter.send(ConverterMsg::AddImg(info))?;
						},
						RenderInfo::Reloaded => tui.set_msg(MessageSetting::Some(BottomMessage::Reloaded)),
						RenderInfo::SearchResults { page_num, num_results } =>
							tui.got_num_results_on_page(page_num, num_results),
					},
					Err(e) => tui.show_error(e),
				}
			}
			Some(img_res) = from_converter.next() => {
				match img_res {
					Ok(ConvertedPage { page, num, num_results, scale_factor }) => {
						tui.page_ready(page, num, num_results, scale_factor);
						if num == tui.page {
							needs_redraw = true;
						}
					},
					Err(e) => tui.show_error(e),
				}
			},
			Some(ipc_page) = async {
				match ipc_page_rx {
					Some(ref mut rx) => rx.next().await,
					None => std::future::pending().await,
				}
			} => {
				tui.page = ipc_page;
				tui.set_msg(MessageSetting::Some(BottomMessage::Error(
					format!("SyncTeX → page {}", ipc_page + 1)
				)));
				// Auto-clear the SyncTeX highlight after 2 seconds (Skim-like flash)
				let clear_tx = to_renderer.clone();
				tokio::spawn(async move {
					tokio::time::sleep(Duration::from_secs(2)).await;
					let _ = clear_tx.send(RenderNotif::ClearSyncTexHighlight);
				});
			},
		};

		let new_area = Tui::main_layout(&term.get_frame(), fullscreen);
		if new_area != main_area {
			main_area = new_area;
			to_renderer.send(RenderNotif::Area(main_area.page_area))?;
			needs_redraw = true;
		}

		if needs_redraw {
			let mut to_display = KittyDisplay::NoChange;
			term.draw(|f| {
				to_display = tui.render(f, &main_area, font_size);
			})?;

			let maybe_err = display_kitty_images(to_display, &mut ev_stream).await;

			if let Err((to_replace, err_desc, enum_err)) = maybe_err {
				match enum_err {
					// This is the error that kitty & ghostty provide us when they delete an
					// image due to memory constraints, so if we get it, we just fix it by
					// re-rendering so it don't display it to the user
					//
					// [TODO] maybe when we detect that an image was deleted, we probe the
					// terminal for the pages around it to see if they were deleted too and if
					// they were, we re-render them? idk
					TransmitError::Terminal(TerminalError::NoEntity(_)) => (),
					_ => tui.set_msg(MessageSetting::Some(BottomMessage::Error(format!(
						"{err_desc}: {enum_err}"
					))))
				}

				for page_num in to_replace {
					tui.page_failed_display(page_num);
					// So that they get re-rendered and sent over again
					to_renderer.send(RenderNotif::PageNeedsReRender(page_num))?;
				}
			}

			execute!(stdout().lock(), EndSynchronizedUpdate)?;
		}
	}
}

fn on_notify_ev(
	to_tui_tx: flume::Sender<Result<RenderInfo, RenderError>>,
	to_render_tx: flume::Sender<RenderNotif>,
	file_name: OsString,
	debounce_delay: Duration
) -> impl Fn(notify::Result<Event>) {
	let last_event: Mutex<Result<(), RenderError>> = Mutex::new(Ok(()));
	let last_event = Arc::new(last_event);

	let debouncer = EventDebouncer::new(debounce_delay, {
		let last_event = last_event.clone();
		move |()| {
			let event = mem::replace(&mut *last_event.lock().unwrap(), Ok(()));
			match event {
				// This shouldn't fail to send unless the receiver gets disconnected. If that's
				// happened, then like the main thread has panicked or something, so it doesn't matter
				// we don't handle the error here.
				Ok(()) => to_render_tx.send(RenderNotif::Reload).unwrap(),
				// If we get an error here, and then an error sending, everything's going wrong. Just give
				// up lol.
				Err(e) => to_tui_tx.send(Err(e)).unwrap()
			}
		}
	});

	move |res| {
		let event = match res {
			Err(e) => Err(RenderError::Notify(e)),

			// TODO: Should we match EventKind::Rename and propogate that so that the other parts of the
			// process know that too? Or should that be
			Ok(ev) => {
				// We only watch the parent directory (see the comment above `watcher.watch` in `fn
				// main`) so we need to filter out events to only ones that pertain to the single file
				// we care about
				if !ev
					.paths
					.iter()
					.any(|path| path.file_name().is_some_and(|f| f == file_name))
				{
					return;
				}

				match ev.kind {
					EventKind::Access(_) => return,
					EventKind::Remove(_) => Err(RenderError::Converting("File was deleted".into())),
					EventKind::Other
					| EventKind::Any
					| EventKind::Create(_)
					| EventKind::Modify(_) => Ok(())
				}
			}
		};
		*last_event.lock().unwrap() = event;
		debouncer.put(());
	}
}

fn parse_color_to_i32(cs: &str) -> Result<i32, csscolorparser::ParseColorError> {
	let color = csscolorparser::parse(cs)?;
	let [r, g, b, _] = color.to_rgba8();
	Ok(i32::from_be_bytes([0, r, g, b]))
}

fn get_font_size_through_stdio() -> Result<(u16, u16), WrappedErr> {
	// send the command code to get the terminal window size
	print!("\x1b[14t");
	std::io::stdout().flush().unwrap();

	// we need to enable raw mode here since this bit of output won't print a newline; it'll
	// just print the info it wants to tell us. So we want to get all characters as they come
	enable_raw_mode().map_err(|e| {
		WrappedErr(
			format!("Can't enable raw mode, which is necessary to receive input: {e}").into()
		)
	})?;

	// read in the returned size until we hit a 't' (which indicates to us it's done)
	let input_vec = BufReader::new(std::io::stdin())
		.bytes()
		.filter_map(Result::ok)
		.take_while(|b| *b != b't')
		.collect::<Vec<_>>();

	// and then disable raw mode again in case we return an error in this next section
	disable_raw_mode().map_err(|e| {
		WrappedErr(format!("Can't put the terminal back into a normal input state: {e}").into())
	})?;

	let input_line = String::from_utf8(input_vec).map_err(|e| {
		WrappedErr(
			format!(
				"The terminal responded to our request for its font size by providing non-utf8 data: {e}"
			)
			.into()
		)
	})?;
	let input_line = input_line
		.trim_start_matches("\x1b[4")
		.trim_start_matches(';');

	// it should input it to us as `\e[4;<height>;<width>t`, so we need to split to get the h/w
	// ignore the first val
	let mut splits = input_line.split([';', 't']);

	let (Some(h), Some(w)) = (splits.next(), splits.next()) else {
		return Err(WrappedErr(
			format!("Terminal responded with unparseable size response '{input_line}'").into()
		));
	};

	let h = h.parse::<u16>().map_err(|e| {
		WrappedErr(
			format!(
				"Your terminal said its height is {h}, but that is not a 16-bit unsigned integer: {e}"
			)
			.into()
		)
	})?;
	let w = w.parse::<u16>().map_err(|e| {
		WrappedErr(
			format!(
				"Your terminal said its width is {w}, but that is not a 16-bit unsigned integer: {e}"
			)
			.into()
		)
	})?;

	Ok((w, h))
}

/// Parse a synctex forward argument in the format "line:col:file" or "line:file"
fn parse_synctex_arg(arg: &str) -> Result<(usize, usize, String), String> {
	// Try line:col:file first
	let parts: Vec<&str> = arg.splitn(3, ':').collect();
	match parts.len() {
		3 => {
			let line = parts[0]
				.parse::<usize>()
				.map_err(|e| format!("invalid line number: {e}"))?;
			let col = parts[1]
				.parse::<usize>()
				.map_err(|e| format!("invalid column number: {e}"))?;
			Ok((line, col, parts[2].to_string()))
		}
		2 => {
			let line = parts[0]
				.parse::<usize>()
				.map_err(|e| format!("invalid line number: {e}"))?;
			Ok((line, 0, parts[1].to_string()))
		}
		_ => Err("expected format line:col:file or line:file".into())
	}
}

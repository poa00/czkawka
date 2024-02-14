use std::rc::Rc;
use std::thread;

use chrono::NaiveDateTime;
use crossbeam_channel::{Receiver, Sender};
use humansize::{format_size, BINARY};
use rayon::prelude::*;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use czkawka_core::common::{split_path, split_path_compare, DEFAULT_THREAD_SIZE};
use czkawka_core::common_dir_traversal::{FileEntry, ProgressData};
use czkawka_core::common_tool::CommonData;
use czkawka_core::common_traits::ResultEntry;
use czkawka_core::empty_files::EmptyFiles;
use czkawka_core::empty_folder::{EmptyFolder, FolderEntry};
use czkawka_core::similar_images;
use czkawka_core::similar_images::{ImagesEntry, SimilarImages};

use crate::common::split_u64_into_i32s;
use crate::settings::{collect_settings, SettingsCustom, ALLOWED_HASH_TYPE_VALUES, ALLOWED_RESIZE_ALGORITHM_VALUES};
use crate::{CurrentTab, GuiState, MainListModel, MainWindow, ProgressToSend};

pub fn connect_scan_button(app: &MainWindow, progress_sender: Sender<ProgressData>, stop_receiver: Receiver<()>) {
    let a = app.as_weak();
    app.on_scan_starting(move |active_tab| {
        let progress_sender = progress_sender.clone();
        let stop_receiver = stop_receiver.clone();
        let app = a.upgrade().unwrap();

        app.set_progress_datas(ProgressToSend {
            all_progress: 0,
            current_progress: -1,
            step_name: "".into(),
        });

        let custom_settings = collect_settings(&app);

        let a = app.as_weak();
        match active_tab {
            CurrentTab::EmptyFolders => {
                scan_empty_folders(a, progress_sender, stop_receiver, custom_settings);
            }
            CurrentTab::EmptyFiles => {
                scan_empty_files(a, progress_sender, stop_receiver, custom_settings);
            }
            CurrentTab::SimilarImages => {
                scan_similar_images(a, progress_sender, stop_receiver, custom_settings);
            }
            CurrentTab::Settings => panic!("Button should be disabled"),
        }
    });
}

fn scan_similar_images(a: Weak<MainWindow>, progress_sender: Sender<ProgressData>, stop_receiver: Receiver<()>, custom_settings: SettingsCustom) {
    thread::Builder::new()
        .stack_size(DEFAULT_THREAD_SIZE)
        .spawn(move || {
            let mut finder = SimilarImages::new();
            set_common_settings(&mut finder, &custom_settings);
            finder.set_hash_size(custom_settings.similar_images_sub_hash_size);
            let resize_algortithm = ALLOWED_RESIZE_ALGORITHM_VALUES
                .iter()
                .find(|(setting_name, _gui_name, _resize_alg)| setting_name == &custom_settings.similar_images_sub_resize_algorithm)
                .expect("Resize algorithm not found")
                .2;
            finder.set_image_filter(resize_algortithm);
            let hash_type = ALLOWED_HASH_TYPE_VALUES
                .iter()
                .find(|(setting_name, _gui_name, _resize_alg)| setting_name == &custom_settings.similar_images_sub_hash_type)
                .expect("Hash type not found")
                .2;
            finder.set_hash_alg(hash_type);
            dbg!(&custom_settings.similar_images_sub_ignore_same_size);
            finder.set_exclude_images_with_same_size(custom_settings.similar_images_sub_ignore_same_size);
            finder.set_similarity(custom_settings.similar_images_sub_similarity as u32);
            finder.find_similar_images(Some(&stop_receiver), Some(&progress_sender));

            if finder.get_use_reference() {
                let mut vector = finder.get_similar_images_referenced().clone();
                let messages = finder.get_text_messages().create_messages_text();

                let hash_size = custom_settings.similar_images_sub_hash_size;

                for (_first_entry, vec_fe) in &mut vector {
                    vec_fe.par_sort_unstable_by_key(|e| e.similarity);
                }

                a.upgrade_in_event_loop(move |app| {
                    write_similar_images_results_referenced(&app, vector, messages, hash_size);
                })
            } else {
                let mut vector = finder.get_similar_images().clone();
                let messages = finder.get_text_messages().create_messages_text();

                let hash_size = custom_settings.similar_images_sub_hash_size;

                for vec_fe in &mut vector {
                    vec_fe.par_sort_unstable_by_key(|e| e.similarity);
                }

                a.upgrade_in_event_loop(move |app| {
                    write_similar_images_results(&app, vector, messages, hash_size);
                })
            }
        })
        .unwrap();
}
fn write_similar_images_results_referenced(app: &MainWindow, vector: Vec<(ImagesEntry, Vec<ImagesEntry>)>, messages: String, hash_size: u8) {
    let items_found = vector.len();
    let items = Rc::new(VecModel::default());
    for (ref_fe, vec_fe) in vector {
        let (data_model_str, data_model_int) = prepare_data_model_similar_images(&ref_fe, hash_size);
        insert_data_to_model(&items, data_model_str, data_model_int, true);

        for fe in vec_fe {
            let (data_model_str, data_model_int) = prepare_data_model_similar_images(&fe, hash_size);
            insert_data_to_model(&items, data_model_str, data_model_int, false);
        }
    }
    app.set_similar_images_model(items.into());
    app.invoke_scan_ended(format!("Found {items_found} similar images files").into());
    app.global::<GuiState>().set_info_text(messages.into());
}
fn write_similar_images_results(app: &MainWindow, vector: Vec<Vec<ImagesEntry>>, messages: String, hash_size: u8) {
    let items_found = vector.len();
    let items = Rc::new(VecModel::default());
    for vec_fe in vector {
        insert_data_to_model(&items, ModelRc::new(VecModel::default()), ModelRc::new(VecModel::default()), true);
        for fe in vec_fe {
            let (data_model_str, data_model_int) = prepare_data_model_similar_images(&fe, hash_size);
            insert_data_to_model(&items, data_model_str, data_model_int, false);
        }
    }
    app.set_similar_images_model(items.into());
    app.invoke_scan_ended(format!("Found {items_found} similar images files").into());
    app.global::<GuiState>().set_info_text(messages.into());
}
fn prepare_data_model_similar_images(fe: &ImagesEntry, hash_size: u8) -> (ModelRc<SharedString>, ModelRc<i32>) {
    let (directory, file) = split_path(fe.get_path());
    let data_model_str = VecModel::from_slice(&[
        similar_images::get_string_from_similarity(&fe.similarity, hash_size).into(),
        format_size(fe.size, BINARY).into(),
        format!("{}x{}", fe.width, fe.height).into(),
        file.into(),
        directory.into(),
        NaiveDateTime::from_timestamp_opt(fe.get_modified_date() as i64, 0).unwrap().to_string().into(),
    ]);
    let modification_split = split_u64_into_i32s(fe.get_modified_date());
    let size_split = split_u64_into_i32s(fe.size);
    let data_model_int = VecModel::from_slice(&[modification_split.0, modification_split.1, size_split.0, size_split.1, fe.width as i32, fe.height as i32]);
    (data_model_str, data_model_int)
}

///////////////////////////////// Empty Files
fn scan_empty_files(a: Weak<MainWindow>, progress_sender: Sender<ProgressData>, stop_receiver: Receiver<()>, custom_settings: SettingsCustom) {
    thread::Builder::new()
        .stack_size(DEFAULT_THREAD_SIZE)
        .spawn(move || {
            let mut finder = EmptyFiles::new();
            set_common_settings(&mut finder, &custom_settings);
            finder.find_empty_files(Some(&stop_receiver), Some(&progress_sender));

            let mut vector = finder.get_empty_files().clone();
            let messages = finder.get_text_messages().create_messages_text();

            vector.par_sort_unstable_by(|a, b| split_path_compare(a.path.as_path(), b.path.as_path()));

            a.upgrade_in_event_loop(move |app| {
                write_empty_files_results(&app, vector, messages);
            })
        })
        .unwrap();
}
fn write_empty_files_results(app: &MainWindow, vector: Vec<FileEntry>, messages: String) {
    let items_found = vector.len();
    let items = Rc::new(VecModel::default());
    for fe in vector {
        let (data_model_str, data_model_int) = prepare_data_model_empty_files(&fe);
        insert_data_to_model(&items, data_model_str, data_model_int, false);
    }
    app.set_empty_files_model(items.into());
    app.invoke_scan_ended(format!("Found {items_found} empty files").into());
    app.global::<GuiState>().set_info_text(messages.into());
}

fn prepare_data_model_empty_files(fe: &FileEntry) -> (ModelRc<SharedString>, ModelRc<i32>) {
    let (directory, file) = split_path(fe.get_path());
    let data_model_str = VecModel::from_slice(&[
        file.into(),
        directory.into(),
        NaiveDateTime::from_timestamp_opt(fe.get_modified_date() as i64, 0).unwrap().to_string().into(),
    ]);
    let modification_split = split_u64_into_i32s(fe.get_modified_date());
    let size_split = split_u64_into_i32s(fe.size);
    let data_model_int = VecModel::from_slice(&[modification_split.0, modification_split.1, size_split.0, size_split.1]);
    (data_model_str, data_model_int)
}

////////////////////////////////////////// Empty Folders
fn scan_empty_folders(a: Weak<MainWindow>, progress_sender: Sender<ProgressData>, stop_receiver: Receiver<()>, custom_settings: SettingsCustom) {
    thread::Builder::new()
        .stack_size(DEFAULT_THREAD_SIZE)
        .spawn(move || {
            let mut finder = EmptyFolder::new();
            set_common_settings(&mut finder, &custom_settings);
            finder.find_empty_folders(Some(&stop_receiver), Some(&progress_sender));

            let mut vector = finder.get_empty_folder_list().values().cloned().collect::<Vec<_>>();
            let messages = finder.get_text_messages().create_messages_text();

            vector.par_sort_unstable_by(|a, b| split_path_compare(a.path.as_path(), b.path.as_path()));

            a.upgrade_in_event_loop(move |app| {
                write_empty_folders_results(&app, vector, messages);
            })
        })
        .unwrap();
}
fn write_empty_folders_results(app: &MainWindow, vector: Vec<FolderEntry>, messages: String) {
    let items_found = vector.len();
    let items = Rc::new(VecModel::default());
    for fe in vector {
        let (data_model_str, data_model_int) = prepare_data_model_empty_folders(&fe);
        insert_data_to_model(&items, data_model_str, data_model_int, false);
    }
    app.set_empty_folder_model(items.into());
    app.invoke_scan_ended(format!("Found {items_found} empty folders").into());
    app.global::<GuiState>().set_info_text(messages.into());
}

fn prepare_data_model_empty_folders(fe: &FolderEntry) -> (ModelRc<SharedString>, ModelRc<i32>) {
    let (directory, file) = split_path(&fe.path);
    let data_model_str = VecModel::from_slice(&[
        file.into(),
        directory.into(),
        NaiveDateTime::from_timestamp_opt(fe.modified_date as i64, 0).unwrap().to_string().into(),
    ]);
    let modification_split = split_u64_into_i32s(fe.get_modified_date());
    let data_model_int = VecModel::from_slice(&[modification_split.0, modification_split.1]);
    (data_model_str, data_model_int)
}

////////////////////////////////////////// Common
fn insert_data_to_model(items: &Rc<VecModel<MainListModel>>, data_model_str: ModelRc<SharedString>, data_model_int: ModelRc<i32>, header_row: bool) {
    let main = MainListModel {
        checked: false,
        header_row,
        selected_row: false,
        val_str: ModelRc::new(data_model_str),
        val_int: ModelRc::new(data_model_int),
    };
    items.push(main);
}

fn set_common_settings<T>(component: &mut T, custom_settings: &SettingsCustom)
where
    T: CommonData,
{
    component.set_included_directory(custom_settings.included_directories.clone());
    component.set_reference_directory(custom_settings.included_directories_referenced.clone());
    component.set_excluded_directory(custom_settings.excluded_directories.clone());
    component.set_recursive_search(custom_settings.recursive_search);
    component.set_minimal_file_size(custom_settings.minimum_file_size as u64 * 1024);
    component.set_maximal_file_size(custom_settings.maximum_file_size as u64 * 1024);
    component.set_allowed_extensions(custom_settings.allowed_extensions.clone());
    component.set_excluded_extensions(custom_settings.excluded_extensions.clone());
    component.set_excluded_items(custom_settings.excluded_items.split(',').map(str::to_string).collect());
    component.set_exclude_other_filesystems(custom_settings.ignore_other_file_systems);
    component.set_use_cache(custom_settings.use_cache);
    component.set_save_also_as_json(custom_settings.save_also_as_json);
}
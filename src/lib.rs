use ab_glyph::FontRef;
use clap::Parser;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{FilterType, resize};
use image::{GenericImage, Rgb, RgbImage, load_from_memory};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;
use rayon::iter::ParallelIterator;
use rayon::prelude::*;
use rexif::ExifTag::*;
use rexif::{ExifEntry, ExifTag, parse_buffer, parse_file};
use std::cmp::min;
use std::fs;
use std::fs::File;
use std::io::ErrorKind::InvalidInput;
use std::io::{BufWriter, Error};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type Empty = Result<()>;
/// L卡口Logo图
static LOGO_BYTES: &[u8] = include_bytes!("../images/logo.jpg");
/// 字体文件
static FONT_BYTES: &[u8] = include_bytes!("../fonts/MiSansLatin-Demibold.ttf");

fn is_image_file(path: &Path) -> bool {
   if let Some(extension) = path.extension() {
      let ext = extension.to_string_lossy().to_lowercase();
      matches!(ext.as_str(), "jpg" | "jpeg")
   } else {
      false
   }
}

fn expand_directory_images(dir_path: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
   let entries = fs::read_dir(dir_path)?;

   for entry in entries {
      let path = entry?.path();
      if path.is_file() && is_image_file(&path) {
         // 如果是图片文件，添加到结果中
         result.push(path);
      } else if path.is_dir() {
         // 如果是目录，递归处理
         expand_directory_images(&path, result)?;
      }
   }
   Ok(())
}

fn expand_directories_images(images: &mut Vec<PathBuf>) -> Result<()> {
   let mut expanded_paths = Vec::new();
   for path in images.drain(..) {
      if path.exists() && path.is_file() && is_image_file(&path) {
         expanded_paths.push(path);
      } else if path.is_dir() {
         expand_directory_images(&path, &mut expanded_paths)?;
      }
   }
   // 替换图片列表
   *images = expanded_paths;
   Ok(())
}

pub fn parse_path(file_path: &PathBuf, target_path: &PathBuf) -> Result<PathBuf> {
   let file_name = file_path
      .file_name()
      .ok_or_else(|| Error::new(InvalidInput, "无效的文件路径"))?;
   // 为文件名添加mark前缀
   let marked_file_name = format!("mark_{}", file_name.to_string_lossy());

   // 判断target_path是否存在
   if !target_path.exists() || !target_path.is_dir() {
      fs::create_dir_all(target_path)?;
   }
   // 拼接target_path和带前缀的文件名
   Ok(target_path.join(marked_file_name))
}

#[derive(Parser)]
#[command(version)]
pub struct LumixMarkCli {
   /// 多张图片地址或者文件夹，使用空格分隔
   pub images: Vec<PathBuf>,
   #[arg(short, long, default_value = ".")]
   /// 输出到指定文件夹，不存在则会创建
   pub target_path: PathBuf,
   #[arg(short, long, default_value_t = 75)]
   /// 图片质量 （75 - 100）
   pub quality: u8,
   #[arg(short, long, default_value_t = 0.14)]
   /// 水印相当于短边的比率（0.1 - 0.15）
   pub ratio: f32,
   /// 并行处理图片数量
   #[arg(short, long, default_value_t = 5)]
   pub par_count: usize,
}

impl LumixMarkCli {
   pub fn parse_image_list() -> Self {
      let mut config = Self::parse();
      expand_directories_images(&mut config.images).unwrap();
      config
   }
   pub fn par_draw_logo_exif_task(&self) {
      self
         .images
         .par_iter()
         .take(self.par_count)
         .for_each(|path| {
            let mut lumix_mark = LumixMark::from_image(path, self.ratio)
               .expect(&format!("当前图片操作失败：{:?}", path));
            lumix_mark
               .draw_logo_exif(
                  0.35,
                  FONT_BYTES,
                  Color::Black,
                  0.45,
                  Color::RGB(50, 50, 50),
                  0.3,
                  0.12,
                  Color::HEX("#969696"),
                  0.01,
                  0.25,
                  LOGO_BYTES,
                  0.35,
                  0.35,
               )
               .unwrap();
            lumix_mark
               .save_with_quality(
                  parse_path(path, &self.target_path).expect(&format!(
                     "读写文件路径失败：target_path:{:?};path:{:?}",
                     &self.target_path, path
                  )),
                  self.quality,
               )
               .expect(&format!(
                  "保存文件失败：target_path:{:?};path:{:?}",
                  &self.target_path, path
               ));
         });
   }
}

pub struct LumixMark {
   pub canvas: RgbImage,
   pub exif: Exif,
   pub mark_area: (u32, u32, u32, u32),
   pub width: u32,
   pub height: u32,
   pub mark_height: f32,
}

impl LumixMark {
   /// # 初始化画布
   ///
   /// # 参数
   /// * `file_path` - 需要添加水印的照片文件路径
   /// * `mark_ratio` - 设置水印高度比例 （水印高度 / 照片最短边）
   pub fn from_image<P: AsRef<Path>>(file_path: P, mark_ratio: f32) -> Result<Self> {
      // 1. 读取图片
      let file_bytes = fs::read(&file_path)?;
      let exif = Exif::from_bytes(&file_bytes)?;
      let original_img = load_from_memory(&file_bytes)?;
      // 根据exif反转图像
      let rgb_img = match exif.orientation.as_str() {
         "Straight" => original_img.to_rgb8(),
         "Rotated to left" => original_img.rotate90().to_rgb8(),
         "Rotated to right" => original_img.rotate270().to_rgb8(),
         _ => original_img.to_rgb8(),
      };
      let (img_width, img_height) = rgb_img.dimensions();
      let mark_height = (min(img_width, img_height) as f32 * mark_ratio) as u32;
      let add_mark_height = img_height + mark_height;
      // 2. 创建画布
      let mut canvas =
         RgbImage::from_pixel(img_width, add_mark_height, Color::White.into());
      canvas.copy_from(&rgb_img, 0, 0)?;
      Ok(Self {
         canvas,
         width: img_width,
         height: add_mark_height,
         mark_height: mark_height as f32,
         mark_area: (0, img_height, img_width, add_mark_height),
         exif,
      })
   }
   /// # 指定质量保存JPEG图片
   ///
   /// # 参数
   /// * `file_name` - 指定保存的文件路径名
   /// * `quality` - 设置保存的图片质量（75 - 100）
   pub fn save_with_quality<P: AsRef<Path>>(&self, file_name: P, quality: u8) -> Empty {
      let file = File::create(file_name)?;
      let writer = BufWriter::new(file);
      let mut encoder = JpegEncoder::new_with_quality(writer, quality);
      encoder.encode_image(&self.canvas)?;
      Ok(())
   }
   /// 绘制Logo和Exif信息到画布
   pub fn draw_logo_exif(
      &mut self,
      padding_ratio: f32,
      font_bytes: &[u8],
      model_color: Color,
      model_text_size_ratio: f32,
      exif_color: Color,
      exif_text_size_ratio: f32,
      gap_ratio: f32,
      rect_color: Color,
      rect_width_ratio: f32,
      rect_height_ratio: f32,
      logo_bytes: &[u8],
      logo_width_ratio: f32,
      logo_height_ratio: f32,
   ) -> Empty {
      let padding = (self.mark_height * padding_ratio) as u32;
      let model_text_size = self.mark_height * model_text_size_ratio;
      let exif_text_size = self.mark_height * exif_text_size_ratio;
      let gap = (self.mark_height * gap_ratio) as i32;
      let rect_width = (self.mark_height * rect_width_ratio) as u32;
      let rect_height = (self.mark_height * rect_height_ratio) as u32;
      let logo_width = (self.mark_height * logo_width_ratio) as u32;
      let logo_height = (self.mark_height * logo_height_ratio) as u32;
      let (start_x, start_y, end_x, end_y) = self.mark_area;
      // 加载字体
      let font = FontRef::try_from_slice(font_bytes)?;
      let (model_width, _) = text_size(model_text_size, &font, &self.exif.model_title);
      println!("计算{}的显示宽度:{}", self.exif.model_title, model_width);
      // 绘制机型
      draw_text_mut(
         &mut self.canvas,
         model_color.into(),
         (start_x + padding) as i32,
         (((start_y + end_y) as f32 - model_text_size) / 2.0) as i32,
         model_text_size,
         &font,
         &self.exif.model_title,
      );
      let exif_text = &self.exif.to_string();
      let (exif_width, _) = text_size(exif_text_size, &font, exif_text);
      println!("计算{exif_text}的显示宽度:{}", exif_width);
      let exif_x = (end_x - exif_width - padding) as i32;
      // 绘制Exif信息
      draw_text_mut(
         &mut self.canvas,
         exif_color.into(),
         exif_x,
         (((start_y + end_y) as f32 - exif_text_size) / 2.0) as i32,
         exif_text_size,
         &font,
         exif_text,
      );
      let rect_x = exif_x - gap - rect_width as i32;
      let rect = Rect::at(
         rect_x,
         ((start_y + end_y - rect_height) as f32 / 2.0) as i32,
      )
      .of_size(rect_width, rect_height);
      // 绘制分隔矩形
      draw_filled_rect_mut(&mut self.canvas, rect, rect_color.into());
      // 加载Logo图片
      let logo = load_from_memory(logo_bytes)?.to_rgb8();
      let resize_logo = resize(&logo, logo_width, logo_height, FilterType::CatmullRom);
      let logo_x = (rect_x - gap) as u32 - logo_width;
      let logo_y = ((start_y + end_y - logo_height) as f32 / 2.0) as u32;
      // 绘制Logo
      self.canvas.copy_from(&resize_logo, logo_x, logo_y)?;
      Ok(())
   }
}

#[derive(Default, Debug)]
pub struct Exif {
   pub model_title: String,
   pub shoot_time: String,
   pub exposure_time: String,
   pub aperture: String,
   pub iso: String,
   pub focal_length: String,
   pub orientation: String,
}

impl Exif {
   /// 从图片文件路径解析EXIF信息
   pub fn from_image<P: AsRef<Path>>(file_path: P) -> Result<Self> {
      let mut exif = Exif::default();
      // 处理所有EXIF条目
      for entry in parse_file(file_path)?.entries {
         Self::process_entry(&mut exif, entry.tag, &entry.value_more_readable, &entry);
      }
      Ok(exif)
   }

   pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
      let mut exif = Exif::default();
      // 处理所有EXIF条目
      for entry in parse_buffer(bytes)?.entries {
         Self::process_entry(&mut exif, entry.tag, &entry.value_more_readable, &entry);
      }
      Ok(exif)
   }

   /// 处理单个EXIF条目，更新Exif结构体字段
   fn process_entry(exif: &mut Exif, tag: ExifTag, value: &str, _entry: &ExifEntry) {
      match tag {
         // 相机型号：处理前缀并修剪空白
         Model => {
            exif.model_title = value.trim().replace("DC-", "LUMIX ");
         }
         // 拍摄时间：直接使用原始值
         DateTimeOriginal => {
            exif.shoot_time = value.to_string();
         }
         // 曝光时间：移除空格并转为大写
         ExposureTime => {
            exif.exposure_time = value.replace(' ', "").to_uppercase();
         }
         // 光圈值：格式化显示
         FNumber => {
            exif.aperture = value.replace("f/", "F");
         }
         // ISO值：直接使用
         ISOSpeedRatings => {
            exif.iso = value.replace(' ', "").to_uppercase();
         }
         FocalLengthIn35mmFilm => {
            if !value.trim().is_empty() {
               exif.focal_length = value.replace(' ', "").to_uppercase();
            }
         }
         // 焦距：格式化显示
         FocalLength => {
            if exif.focal_length.is_empty() {
               exif.focal_length = value.replace(' ', "").to_uppercase();
            }
         }
         Orientation => {
            exif.orientation = value.into();
         }
         // 忽略其他标签
         _ => {}
      }
   }

   pub fn to_string(&self) -> String {
      format!(
         "{} {} {} {}",
         self.focal_length, self.aperture, self.exposure_time, self.iso
      )
   }
}

pub enum Color {
   Black,
   White,
   RGB(u8, u8, u8),
   HEX(&'static str),
}

impl From<Color> for Rgb<u8> {
   fn from(color: Color) -> Self {
      match color {
         Color::Black => Rgb([0, 0, 0]),
         Color::White => Rgb([255, 255, 255]),
         Color::RGB(r, g, b) => Rgb([r, g, b]),
         Color::HEX(hex) => {
            let hex = hex.trim_start_matches('#');
            if hex.len() != 6 {
               return Rgb([0, 0, 0]);
            }
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or_default();
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or_default();
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or_default();
            Rgb([r, g, b])
         }
      }
   }
}

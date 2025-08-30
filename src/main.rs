use lumix_mark::LumixMarkCli;

fn main() {
   let cli = LumixMarkCli::parse_image_list();
   cli.par_draw_logo_exif_task();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("src/ui/app.slint")?;
    Ok(())
}

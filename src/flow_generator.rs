// Flow generator
//
// Basically this provides a type-safe way to generate ComfyUI node graphs.

struct Flow {

}

struct NodeReference {
    id: usize,
}

enum Literal {
    String(String),
    Number(f64),
    }


impl Flow {
    pub fn new() -> Self {
        Self {

        }
    }

//    pub fn add
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_txt2img() {
        let mut flow = Flow::new();

        let model = flow.model_loader("anime.safetensors");
        let positive_prompt = model.clip_encoder("I love anime");
        let negative_prompt = model.clip_encoder("photograph");
        let sampler = model.sample();
    }
}